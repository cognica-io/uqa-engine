//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{providers::destination, *};
use crate::{tests::providers::Provider, SQLFunctionOptions, SQLFunctionRegistration};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

fn register_abs(engine: &Engine, value: i64) -> (SQLFunctionRegistration, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let handle = engine
        .register_scalar_function_retained(
            "abs",
            SQLFunctionOptions::default(),
            move |_: &[Value]| {
                observed.fetch_add(1, Ordering::SeqCst);
                Ok(Value::Int(value))
            },
        )
        .unwrap();
    (handle, calls)
}

fn values(engine: &Engine) -> (Value, Value) {
    let result = engine
        .sql(
            "SELECT a.v AS fixed, b.v AS chosen FROM saved_builtin a CROSS JOIN saved_host b",
            &[],
        )
        .unwrap();
    let row = &result.rows[0];
    (row["fixed"].clone(), row["chosen"].clone())
}

fn source_views(engine: &Engine) -> (SQLFunctionRegistration, Arc<AtomicUsize>) {
    engine
        .sql("CREATE VIEW saved_builtin AS SELECT abs(-5) AS v", &[])
        .unwrap();
    let registration = register_abs(engine, 17);
    engine
        .sql(
            "CREATE VIEW saved_host AS SELECT abs(-5) AS v;
             CREATE MATERIALIZED VIEW saved_materialized AS SELECT abs(-5)::integer AS v",
            &[],
        )
        .unwrap();
    assert_eq!(values(engine), (Value::Int(5), Value::Int(17)));
    registration
}

fn verify_materialized(engine: &Engine, refreshed: i64) {
    let read = || {
        engine
            .sql("SELECT v FROM saved_materialized", &[])
            .unwrap()
            .rows[0]["v"]
            .clone()
    };
    assert_eq!(read(), Value::Int(17));
    engine
        .sql("REFRESH MATERIALIZED VIEW saved_materialized", &[])
        .unwrap();
    assert_eq!(read(), Value::Int(refreshed));
}

fn assert_binding_failure(
    job: Result<RestoreJob<'_>, crate::SQLError>,
    path: &Path,
    control: &StorageReadControl,
) {
    match job {
        Ok(job) => assert_unusable(job, "abs"),
        Err(error) => assert!(error.to_string().contains("abs"), "{error}"),
    }
    assert!(!path.exists());
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn standalone_restore_preserves_the_function_selected_at_the_source_cut(
    #[case] provider: Provider,
) {
    let files = tempfile::tempdir().unwrap();
    let store = repository(&files.path().join("archive"));
    let source = provider.open(&files.path().join("source"));
    let (mut registered, calls) = source_views(&source);
    source
        .register_scalar_function("unused_at_cut", |_: &[Value]| Ok(Value::Int(99)))
        .unwrap();
    let parent = capture(&source, &store, None);
    let before = calls.load(Ordering::SeqCst);
    let mut options = BackupOptions::new(32 << 20, 16 << 20);
    options.parent = Some(parent.backup());
    let control = StorageReadControl::with_limit(32 << 20);
    let active = source.begin_backup(&store, options, &control).unwrap();
    registered.unregister().unwrap();
    assert_eq!(values(&source), (Value::Int(5), Value::Int(5)));
    let active = finish_capture(active);
    let retired = capture(&source, &store, Some(active.backup()));
    assert_eq!(before, calls.load(Ordering::SeqCst));
    drop(source);
    store.remove_backup(parent.backup(), &control).unwrap();

    let limits = RestoreOptions::new(16 << 20, 8 << 20);
    let authority = Engine::new();
    let missing = files.path().join("missing");
    assert_binding_failure(
        authority.begin_restore(
            &store,
            active.backup(),
            destination(provider, &missing),
            limits,
            &control,
        ),
        &missing,
        &control,
    );
    let application = Engine::new();
    let (_binding, calls) = register_abs(&application, 31);
    let mut matched = authority
        .begin_restore(
            &store,
            active.backup(),
            destination(provider, &files.path().join("matched")),
            limits.with_registered_functions(&application),
            &control,
        )
        .unwrap();
    prepare(&mut matched);
    let restored = matched.publish().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(values(&restored), (Value::Int(5), Value::Int(31)));
    verify_materialized(&restored, 31);
    drop((restored, matched));
    store.remove_backup(active.backup(), &control).unwrap();

    let mut fallback = authority
        .begin_restore(
            &store,
            retired.backup(),
            destination(provider, &files.path().join("fallback")),
            limits,
            &control,
        )
        .unwrap();
    prepare(&mut fallback);
    let restored = fallback.publish().unwrap();
    assert_eq!(values(&restored), (Value::Int(5), Value::Int(5)));
    verify_materialized(&restored, 5);
    drop((restored, fallback));
    let conflict = files.path().join("conflict");
    assert_binding_failure(
        authority.begin_restore(
            &store,
            retired.backup(),
            destination(provider, &conflict),
            limits.with_registered_functions(&application),
            &control,
        ),
        &conflict,
        &control,
    );
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative, true)]
#[case::sqlite_kv(Provider::SQLiteKeyValue, true)]
#[case::redb(Provider::Redb, true)]
#[case::sqlite_native_builtin(Provider::SQLiteNative, false)]
#[case::sqlite_kv_builtin(Provider::SQLiteKeyValue, false)]
#[case::redb_builtin(Provider::Redb, false)]
fn prepared_restore_keeps_the_function_requirement_after_archive_removal(
    #[case] provider: Provider,
    #[case] requires_registration: bool,
) {
    let files = tempfile::tempdir().unwrap();
    let store = repository(&files.path().join("archive"));
    let source = provider.open(&files.path().join("source"));
    let (mut registered, _) = source_views(&source);
    if !requires_registration {
        registered.unregister().unwrap();
    }
    let root = capture(&source, &store, None);
    drop((registered, source));
    let limits = RestoreOptions::new(16 << 20, 8 << 20);
    let control = StorageReadControl::with_limit(32 << 20);
    let target = files.path().join("restored");
    let previous = Engine::new();
    let previous_binding = requires_registration.then(|| register_abs(&previous, 23));
    let mut job = previous
        .begin_restore(
            &store,
            root.backup(),
            destination(provider, &target),
            limits.with_registered_functions(&previous),
            &control,
        )
        .unwrap();
    prepare(&mut job);
    let request = job.id().unwrap();
    if let Some((_, calls)) = &previous_binding {
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    drop((job, previous_binding, previous));
    store.remove_backup(root.backup(), &control).unwrap();

    let incompatible = Engine::new();
    let conflicting = (!requires_registration).then(|| register_abs(&incompatible, 29));
    let mut rejected = incompatible
        .resume_restore(
            &store,
            request,
            destination(provider, &target),
            limits.with_registered_functions(&incompatible),
            &control,
        )
        .unwrap();
    let failure = rejected.publish().err().expect("incompatible registration");
    let diagnostic = failure.to_string();
    assert!(
        diagnostic.contains("function") || diagnostic.contains("abs"),
        "{diagnostic}"
    );
    if let Some((_, calls)) = &conflicting {
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    drop((failure, rejected, conflicting, incompatible));
    assert!(!target.exists());
    assert_eq!(control.memory().used(), 0);
    let next = Engine::new();
    let current = requires_registration.then(|| register_abs(&next, 47));
    let mut job = next
        .resume_restore(
            &store,
            request,
            destination(provider, &target),
            limits.with_registered_functions(&next),
            &control,
        )
        .unwrap();
    let engine = job.publish().unwrap();
    if let Some((_, calls)) = &current {
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    let expected = if requires_registration { 47 } else { 5 };
    assert_eq!(values(&engine), (Value::Int(5), Value::Int(expected)));
    drop((engine, job));
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn archives_without_function_requirements_keep_known_builtin_bindings(#[case] provider: Provider) {
    let files = tempfile::tempdir().unwrap();
    let store = repository(&files.path().join("archive"));
    let source = provider.open(&files.path().join("source"));
    source
        .sql("CREATE VIEW saved AS SELECT abs(-5) AS v", &[])
        .unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let image = match &source.storage.provider {
        Some(storage) => storage.capture_backup_image(&control),
        None => crate::backup::memory::capture(&source, &control),
    }
    .unwrap();
    let root = finish_capture(
        store
            .begin_image_backup(image, None, 32 << 20, 16 << 20, &control)
            .unwrap(),
    );
    drop(source);
    let next = Engine::new();
    let (_binding, calls) = register_abs(&next, 23);
    let mut job = next
        .begin_restore(
            &store,
            root.backup(),
            destination(provider, &files.path().join("installed")),
            RestoreOptions::new(16 << 20, 8 << 20).with_registered_functions(&next),
            &control,
        )
        .unwrap();
    prepare(&mut job);
    let engine = job.publish().unwrap();
    assert_eq!(
        engine.sql("SELECT v FROM saved", &[]).unwrap().rows[0]["v"],
        Value::Int(5)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop((engine, job));
    assert_eq!(control.memory().used(), 0);
}
