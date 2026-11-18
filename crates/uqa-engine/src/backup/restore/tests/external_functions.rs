//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{providers::destination, *};
use crate::{
    tests::providers::Provider, SQLFunctionOptions, SQLFunctionRegistrationError, SQLTableFunction,
    SQLTableFunctionResult,
};
use std::sync::atomic::{AtomicUsize, Ordering};

fn assert_unusable(mut job: RestoreJob<'_>, expected: &str) {
    let error = loop {
        match job.advance(4096) {
            Err(error) => break error,
            Ok(RestorePhase::Reading | RestorePhase::Validating) => (),
            Ok(phase) => panic!("unusable function descriptor reached {phase:?}"),
        }
    };
    assert!(error.to_string().contains(expected), "{error}");
    assert_eq!(job.phase(), RestorePhase::Failed);
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
}

struct Rows {
    columns: [String; 1],
    metadata_reads: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    value: i64,
}

impl SQLTableFunction for Rows {
    fn output_columns(&self) -> Option<&[String]> {
        self.metadata_reads.fetch_add(1, Ordering::SeqCst);
        Some(&self.columns)
    }

    fn call(&self, _: &[Value]) -> Result<SQLTableFunctionResult, SQLError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(SQLTableFunctionResult::new(
            self.columns.clone(),
            vec![vec![Value::Int(self.value)]],
        ))
    }
}

fn install_rows(engine: &Engine, column: &str, value: i64) -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let metadata_reads = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    engine
        .register_table_function(
            "external_rows",
            Rows {
                columns: [column.into()],
                metadata_reads: Arc::clone(&metadata_reads),
                calls: Arc::clone(&calls),
                value,
            },
        )
        .unwrap();
    (metadata_reads, calls)
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restore_binds_application_functions_without_executing_callbacks(#[case] provider: Provider) {
    let files = tempfile::tempdir().unwrap();
    let repository = repository(&files.path().join("repository"));
    let source = provider.open(&files.path().join("source"));
    let (_, source_calls) = install_rows(&source, "entry", 17);
    source.sql("CREATE FUNCTION saved_rows() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT entry::integer FROM external_rows(); END", &[]).unwrap();
    assert_eq!(
        source.sql("SELECT saved_rows() AS v", &[]).unwrap().rows[0]["v"],
        Value::Int(17)
    );
    let root = capture(&source, &repository, None);
    drop(source);
    assert_eq!(source_calls.load(Ordering::SeqCst), 1);

    let authority = Engine::new();
    install_rows(&authority, "entry", 101);
    let target = files.path().join("destination");
    let control = StorageReadControl::with_limit(32 << 20);
    let limits = RestoreOptions::new(16 << 20, 8 << 20);
    let wrong = Engine::new();
    let (metadata, calls) = install_rows(&wrong, "other", 99);
    for (configuration, expected) in [
        (limits.into(), "external_rows"),
        (limits.with_registered_functions(&wrong), "entry"),
    ] {
        let job = authority
            .begin_restore(
                &repository,
                root.backup(),
                destination(provider, &target),
                configuration,
                &control,
            )
            .unwrap();
        assert_unusable(job, expected);
        assert!(!target.exists());
        assert_eq!(control.memory().used(), 0);
    }
    assert_eq!(metadata.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let application = Engine::new();
    let (metadata, calls) = install_rows(&application, "entry", 23);
    let mut registration = application
        .register_scalar_function_retained(
            "independent",
            SQLFunctionOptions::default(),
            |_: &[Value]| Ok(Value::Int(1)),
        )
        .unwrap();
    let mut job = authority
        .begin_restore(
            &repository,
            root.backup(),
            destination(provider, &target),
            limits.with_registered_functions(&application),
            &control,
        )
        .unwrap();
    drop((authority, application));
    assert!(matches!(
        registration.unregister(),
        Err(SQLFunctionRegistrationError::Busy)
    ));
    prepare(&mut job);
    assert!(!target.exists());
    assert!(matches!(
        registration.unregister(),
        Err(SQLFunctionRegistrationError::Busy)
    ));
    let restored = job.publish().unwrap();
    registration.unregister().unwrap();
    assert_eq!(metadata.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        restored.sql("SELECT saved_rows() AS v", &[]).unwrap().rows[0]["v"],
        Value::Int(23)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    if !matches!(provider, Provider::Memory) {
        restored.sql("BEGIN", &[]).unwrap();
        let sibling = restored.new_session().unwrap();
        assert_eq!(
            sibling.sql("SELECT saved_rows() AS v", &[]).unwrap().rows[0]["v"],
            Value::Int(23)
        );
        drop(sibling);
        restored.sql("ROLLBACK", &[]).unwrap();
        verify_maintenance(&restored);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
    drop((restored, job));
    assert_eq!(control.memory().used(), 0);
}

fn verify_maintenance(engine: &Engine) {
    use std::time::{Duration, Instant};
    engine.sql("CREATE TABLE sampled(v integer); INSERT INTO sampled SELECT n FROM generate_series(1,100) g(n)", &[]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let catalog = engine.storage.catalog.as_ref().unwrap();
    loop {
        if catalog
            .load_column_stats("public.sampled")
            .unwrap()
            .first()
            .is_some_and(|row| row.row_count == 100)
        {
            break;
        }
        let status = engine.automatic_statistics_status();
        assert!(
            status.last_error.is_none() && Instant::now() < deadline,
            "{status:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn install_scalar(engine: &Engine, value: i64) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    engine
        .register_scalar_function("external_scalar", move |_: &[Value]| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Int(value))
        })
        .unwrap();
    calls
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restore_preserves_recursive_views_and_deferred_function_resolution(#[case] provider: Provider) {
    let files = tempfile::tempdir().unwrap();
    let repository = repository(&files.path().join("repository"));
    let source = provider.open(&files.path().join("source"));
    source
        .sql(
            "CREATE VIEW circular AS SELECT 1 AS v;
        CREATE OR REPLACE VIEW circular AS SELECT abs(v) AS v FROM circular;
        SET check_function_bodies = off;
        CREATE FUNCTION lazy_external() RETURNS integer LANGUAGE SQL AS 'SELECT missing_later(1)';
        CREATE VIEW delayed AS SELECT lazy_external() AS v",
            &[],
        )
        .unwrap();
    let expected = ["SELECT v FROM circular", "SELECT v FROM delayed"].map(|sql| {
        let error = source.sql(sql, &[]).unwrap_err();
        (sql, error.sqlstate().unwrap().to_owned(), error.to_string())
    });
    let root = capture(&source, &repository, None);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut job = Engine::new()
        .begin_restore(
            &repository,
            root.backup(),
            destination(provider, &files.path().join("restored")),
            RestoreOptions::new(16 << 20, 8 << 20),
            &control,
        )
        .unwrap();
    prepare(&mut job);
    let restored = job.publish().unwrap();
    for (sql, state, message) in expected {
        let actual = restored.sql(sql, &[]).unwrap_err();
        assert_eq!(actual.sqlstate(), Some(state.as_str()));
        assert_eq!(actual.to_string(), message);
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restore_resolves_function_calls_in_stored_views_and_defaults(#[case] provider: Provider) {
    let files = tempfile::tempdir().unwrap();
    let repository = repository(&files.path().join("repository"));
    let control = StorageReadControl::with_limit(32 << 20);
    let limits = RestoreOptions::new(16 << 20, 8 << 20);
    let application = Engine::new();
    let installed = install_scalar(&application, 23);
    let authority = Engine::new();
    for (index, (declaration, exercise)) in [
        ("CREATE VIEW saved AS SELECT external_scalar(10)::integer AS v", "SELECT v FROM saved"),
        ("CREATE TABLE saved(v integer DEFAULT external_scalar(10)::integer)", "INSERT INTO saved DEFAULT VALUES RETURNING v"),
        ("CREATE DOMAIN saved AS integer DEFAULT external_scalar(10)::integer; CREATE TABLE use_domain(v saved)", "INSERT INTO use_domain DEFAULT VALUES RETURNING v"),
        ("CREATE MATERIALIZED VIEW saved AS SELECT external_scalar(10)::integer AS v", "REFRESH MATERIALIZED VIEW saved; SELECT v FROM saved"),
    ].iter().enumerate() {
        let source = provider.open(&files.path().join(format!("source-{index}")));
        let _source_calls = install_scalar(&source, 17);
        source.sql(declaration, &[]).unwrap();
        assert_eq!(source.sql(exercise, &[]).unwrap().rows[0]["v"], Value::Int(17));
        let root = capture(&source, &repository, None);
        drop(source);
        let target = files.path().join(format!("destination-{index}"));
        let missing = authority.begin_restore(&repository, root.backup(), destination(provider, &target), limits, &control).unwrap();
        assert_unusable(missing, "external_scalar");
        assert!(!target.exists());
        let before = installed.load(Ordering::SeqCst);
        let mut job = authority.begin_restore(&repository, root.backup(), destination(provider, &target), limits.with_registered_functions(&application), &control).unwrap();
        prepare(&mut job);
        let restored = job.publish().unwrap();
        assert_eq!(installed.load(Ordering::SeqCst), before);
        assert_eq!(restored.sql(exercise, &[]).unwrap().rows[0]["v"], Value::Int(23));
        drop((restored, job));
        assert_eq!(control.memory().used(), 0);
    }
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn resumed_prepared_restore_checks_new_process_functions_before_publication(
    #[case] provider: Provider,
) {
    let files = tempfile::tempdir().unwrap();
    let repository = repository(&files.path().join("repository"));
    let source = provider.open(&files.path().join("source"));
    install_scalar(&source, 17);
    source.sql("CREATE FUNCTION saved() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT external_scalar(10)::integer; END; CREATE TABLE events(v integer)", &[]).unwrap();
    let root = capture(&source, &repository, None);
    drop(source);
    let target = files.path().join("destination");
    let control = StorageReadControl::with_limit(32 << 20);
    let limits = RestoreOptions::new(16 << 20, 8 << 20);
    let previous = Engine::new();
    let calls = install_scalar(&previous, 23);
    let mut job = previous
        .begin_restore(
            &repository,
            root.backup(),
            destination(provider, &target),
            limits.with_registered_functions(&previous),
            &control,
        )
        .unwrap();
    prepare(&mut job);
    let id = job.id().unwrap();
    drop((job, previous));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    repository.remove_backup(root.backup(), &control).unwrap();
    let authority = Engine::new();
    let mut missing = authority
        .resume_restore(
            &repository,
            id,
            destination(provider, &target),
            limits,
            &control,
        )
        .unwrap();
    let error = missing.publish().err().unwrap();
    assert!(error.to_string().contains("external_scalar"), "{error}");
    drop(missing);
    assert!(!target.exists());
    let current = Engine::new();
    let calls = install_scalar(&current, 31);
    let mut recovered = authority
        .resume_restore(
            &repository,
            id,
            destination(provider, &target),
            limits.with_registered_functions(&current),
            &control,
        )
        .unwrap();
    let engine = recovered.publish().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    engine
        .sql("INSERT INTO events VALUES(saved())", &[])
        .unwrap();
    drop((engine, recovered, current));
    let later = Engine::new();
    install_scalar(&later, 47);
    let complete = authority
        .cancel_restore(
            &repository,
            id,
            destination(provider, &target),
            limits.with_registered_functions(&later),
            &control,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        complete
            .sql("SELECT v, saved() AS now FROM events", &[])
            .unwrap()
            .rows[0]["v"],
        Value::Int(31)
    );
    assert_eq!(
        complete.sql("SELECT saved() AS now", &[]).unwrap().rows[0]["now"],
        Value::Int(47)
    );
    drop(complete);
    assert_eq!(control.memory().used(), 0);
}
