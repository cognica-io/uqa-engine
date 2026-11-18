//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[derive(Clone, Copy, Debug)]
enum Event {
    Trigger,
    RuleAction,
    RuleCondition,
}

impl Event {
    fn define(self, engine: &Engine) {
        engine
            .sql(
                "CREATE TABLE source_rows(v integer); CREATE TABLE event_log(v integer)",
                &[],
            )
            .unwrap();
        let declaration = match self {
            Self::Trigger => {
                "CREATE FUNCTION log_event() RETURNS trigger LANGUAGE plpgsql AS $$
                 BEGIN INSERT INTO event_log VALUES (NEW.v); RETURN NEW; END $$;
                 CREATE TRIGGER chosen BEFORE INSERT ON source_rows FOR EACH ROW
                 WHEN (abs(NEW.v) > 10) EXECUTE FUNCTION log_event()"
            }
            Self::RuleAction => {
                "CREATE RULE chosen AS ON INSERT TO source_rows DO ALSO
                 INSERT INTO event_log VALUES (abs(NEW.v)::integer)"
            }
            Self::RuleCondition => {
                "CREATE RULE chosen AS ON INSERT TO source_rows WHERE abs(NEW.v) > 10
                 DO ALSO INSERT INTO event_log VALUES (NEW.v)"
            }
        };
        engine.sql(declaration, &[]).unwrap();
    }

    fn write(self, engine: &Engine, selected: i64) {
        engine
            .sql(
                "DELETE FROM event_log; INSERT INTO source_rows VALUES(2)",
                &[],
            )
            .unwrap();
        let rows = engine.sql("SELECT v FROM event_log", &[]).unwrap().rows;
        let expected = match self {
            Self::Trigger | Self::RuleCondition => (selected > 10).then_some(2),
            Self::RuleAction => Some(selected),
        };
        assert_eq!(rows.len(), usize::from(expected.is_some()));
        if let Some(value) = expected {
            assert_eq!(rows[0]["v"], Value::Int(value));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    Builtin,
    Application,
    Retired,
}

impl Origin {
    fn archive(self, event: Event, source: &Engine, store: &LocalRepository) -> BackupRoot {
        if self == Self::Builtin {
            event.define(source);
        }
        let (mut registered, calls) = register_abs(source, 17);
        if self != Self::Builtin {
            event.define(source);
        }
        event.write(source, if self == Self::Builtin { 2 } else { 17 });
        let before = calls.load(Ordering::SeqCst);
        assert_eq!(before > 0, self != Self::Builtin);
        if self == Self::Retired {
            registered.unregister().unwrap();
            event.write(source, 2);
        }
        let root = capture(source, store, None);
        assert_eq!(calls.load(Ordering::SeqCst), before);
        root
    }

    fn install(
        self,
        engine: &Engine,
        value: i64,
    ) -> Option<(SQLFunctionRegistration, Arc<AtomicUsize>)> {
        (self != Self::Retired).then(|| register_abs(engine, value))
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_event_writes_follow_the_function_selected_at_capture(
    #[case] provider: Provider,
    #[values(Event::Trigger, Event::RuleAction, Event::RuleCondition)] event: Event,
    #[values(Origin::Builtin, Origin::Application, Origin::Retired)] origin: Origin,
) {
    let files = tempfile::tempdir().unwrap();
    let store = repository(&files.path().join("archive"));
    let source = provider.open(&files.path().join("source"));
    let root = origin.archive(event, &source, &store);
    drop(source);

    let limits = RestoreOptions::new(16 << 20, 8 << 20);
    let control = StorageReadControl::with_limit(32 << 20);
    let alternate = Engine::new();
    let alternate_binding = (origin == Origin::Retired).then(|| register_abs(&alternate, 41));
    let alternate_target = files.path().join("alternate");
    let job = alternate.begin_restore(
        &store,
        root.backup(),
        destination(provider, &alternate_target),
        limits.with_registered_functions(&alternate),
        &control,
    );
    if origin == Origin::Builtin {
        let mut job = job.unwrap();
        prepare(&mut job);
        event.write(&job.publish().unwrap(), 2);
    } else {
        assert_binding_failure(job, &alternate_target, &control);
    }

    let runtime = Engine::new();
    let registered = origin.install(&runtime, 31);
    let mut recovered = None;
    let target = files.path().join("matched");
    let mut job = runtime
        .begin_restore(
            &store,
            root.backup(),
            destination(provider, &target),
            limits.with_registered_functions(&runtime),
            &control,
        )
        .unwrap();
    prepare(&mut job);
    if let Some((_, calls)) = &registered {
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    assert_eq!(job.id().is_some(), !matches!(provider, Provider::Memory));
    if let Some(request) = job.id() {
        drop(job);
        store.remove_backup(root.backup(), &control).unwrap();
        if origin != Origin::Builtin {
            let mut incompatible = alternate
                .resume_restore(
                    &store,
                    request,
                    destination(provider, &target),
                    limits.with_registered_functions(&alternate),
                    &control,
                )
                .unwrap();
            let diagnostic = incompatible
                .publish()
                .err()
                .expect("incompatible event function")
                .to_string();
            assert!(
                diagnostic.contains("function") || diagnostic.contains("abs"),
                "{diagnostic}"
            );
            drop(incompatible);
            assert!(!target.exists());
            assert_eq!(control.memory().used(), 0);
        }
        let next = Engine::new();
        recovered = origin.install(&next, 47);
        job = next
            .resume_restore(
                &store,
                request,
                destination(provider, &target),
                limits.with_registered_functions(&next),
                &control,
            )
            .unwrap();
    }
    let restored = job.publish().unwrap();
    for (_, calls) in registered
        .iter()
        .chain(&recovered)
        .chain(&alternate_binding)
    {
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    let initial = match origin {
        Origin::Application if recovered.is_some() => 47,
        Origin::Application => 31,
        Origin::Retired | Origin::Builtin => 2,
    };
    event.write(&restored, initial);
    let (mut replacement, _) = register_abs(&restored, 7);
    event.write(&restored, if origin == Origin::Builtin { 2 } else { 7 });
    replacement.unregister().unwrap();
    event.write(&restored, 2);
    drop((replacement, registered, recovered, restored, job));
    assert_eq!(control.memory().used(), 0);
}
