//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restore_preserves_late_function_lookup_after_source_registration_retirement(
    #[case] provider: Provider,
    #[values(false, true)] routine: bool,
) {
    let files = tempfile::tempdir().unwrap();
    let store = repository(&files.path().join("archive"));
    let source = provider.open(&files.path().join("source"));
    source
        .sql("CREATE VIEW fixed AS SELECT abs(-5)::integer AS v", &[])
        .unwrap();
    let (mut registered, calls) = register_abs(&source, 17);
    let (definition, query, factor) = if routine {
        (
            "CREATE FUNCTION saved(v integer DEFAULT abs(-5)::integer) RETURNS integer
             LANGUAGE SQL BEGIN ATOMIC SELECT v + abs(-5)::integer; END",
            "SELECT saved() AS v",
            2,
        )
    } else {
        (
            "CREATE TABLE saved(v integer DEFAULT abs(-5)::integer,
             CONSTRAINT saved_check CHECK (v = abs(-5)))",
            "INSERT INTO saved DEFAULT VALUES RETURNING v",
            1,
        )
    };
    source.sql(definition, &[]).unwrap();
    let check = |engine: &Engine, expected: i64| {
        assert_eq!(
            engine.sql(query, &[]).unwrap().rows[0]["v"],
            Value::Int(expected * factor)
        );
        assert_eq!(
            engine.sql("SELECT v FROM fixed", &[]).unwrap().rows[0]["v"],
            Value::Int(5)
        );
    };
    check(&source, 17);
    registered.unregister().unwrap();
    let before = calls.load(Ordering::SeqCst);
    check(&source, 5);
    let root = capture(&source, &store, None);
    assert_eq!(calls.load(Ordering::SeqCst), before);
    let (_replacement, _) = register_abs(&source, 42);
    check(&source, 42);
    drop(source);

    let authority = Engine::new();
    let target = files.path().join("restored");
    let options = RestoreOptions::new(16 << 20, 8 << 20);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut job = authority
        .begin_restore(
            &store,
            root.backup(),
            destination(provider, &target),
            options,
            &control,
        )
        .unwrap();
    prepare(&mut job);
    if let Some(request) = job.id() {
        drop(job);
        store.remove_backup(root.backup(), &control).unwrap();
        job = authority
            .resume_restore(
                &store,
                request,
                destination(provider, &target),
                options,
                &control,
            )
            .unwrap();
    }
    let restored = job.publish().unwrap();
    check(&restored, 5);
    let (mut installed, _) = register_abs(&restored, 42);
    check(&restored, 42);
    installed.unregister().unwrap();
    check(&restored, 5);
    drop((restored, job));
    assert_eq!(control.memory().used(), 0);
}
