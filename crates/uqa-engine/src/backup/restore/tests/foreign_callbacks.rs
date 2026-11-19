//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::foreign_bindings::{assert_rejected, begin_provider, run_wrapper_declarations};
use super::providers::capture_provider;
use super::*;
use crate::tests::providers::Provider;
use uqa_execution::schema::foreign_creation::{ForeignCreationRegistry, ForeignRegistryReads};

#[derive(Clone, Copy, Debug)]
enum Contradiction {
    Oid,
    ArgumentType,
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restore_rejects_foreign_callbacks_that_disagree_with_the_live_sql_routine(
    #[case] provider: Provider,
    #[values(Contradiction::Oid, Contradiction::ArgumentType)] change: Contradiction,
) {
    let files = tempfile::tempdir().unwrap();
    let source = provider.open(&files.path().join("source"));
    source
        .sql(
            "CREATE FUNCTION live_validator(text[],oid) RETURNS integer LANGUAGE SQL RETURN 1;
                CREATE FOREIGN DATA WRAPPER live_fdw VALIDATOR live_validator",
            &[],
        )
        .unwrap();
    let mut wrapper = source.wrappers()["live_fdw"].clone();
    let validator = wrapper.validator.as_mut().unwrap();
    match change {
        Contradiction::Oid => validator.oid += 1,
        Contradiction::ArgumentType => validator.binding.argument_types[0] = "integer[]".into(),
    }
    let repository = repository(&files.path().join("repository"));
    let expected = "live function disagrees with its retained identity";
    let root = if let Some(persistent) = source.storage.provider.clone() {
        drop(source);
        let session = persistent.open_session().unwrap();
        uqa_execution::catalog::foreign::wrappers::persist(session.catalog.as_ref(), &wrapper)
            .unwrap();
        drop(session);
        let error = Engine::from_persistent_provider(persistent.clone())
            .err()
            .expect("ordinary open must reject the same callback contradiction");
        assert!(error.to_string().contains(expected), "{error}");
        capture_provider(persistent.as_ref(), &repository)
    } else {
        source
            .wrappers_write()
            .insert(wrapper.name.clone(), wrapper)
            .unwrap();
        capture(&source, &repository, None)
    };
    assert_rejected(
        provider,
        &repository,
        root,
        &files.path().join("restored"),
        expected,
    );
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restore_preserves_retired_sql_validators_and_invokes_live_callbacks_only_on_use(
    #[case] provider: Provider,
) {
    let files = tempfile::tempdir().unwrap();
    let source = provider.open(&files.path().join("source"));
    run_wrapper_declarations(
        &source,
        &[
            "self_removing_validator",
            "self_removing_wrapper",
            "self_removing_replacement",
        ],
    );
    let old_oid = source.wrappers()["orphan_fdw"]
        .validator
        .as_ref()
        .unwrap()
        .oid;
    source.sql("CREATE TABLE validator_calls(kind oid);
                CREATE FUNCTION live_validator(text[],oid) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN INSERT INTO validator_calls VALUES ($2); RETURN 1; END $$;
                CREATE FOREIGN DATA WRAPPER live_fdw VALIDATOR live_validator", &[]).unwrap();
    let calls = "SELECT kind::integer FROM validator_calls ORDER BY kind";
    let before = source.sql(calls, &[]).unwrap().rows;
    assert_eq!(before.len(), 1);
    let repository = repository(&files.path().join("repository"));
    let root = capture(&source, &repository, None);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut job = begin_provider(
        provider,
        &repository,
        root,
        &files.path().join("restored"),
        &control,
    );
    prepare(&mut job);
    let restored = job.publish().unwrap();
    drop(job);
    assert_eq!(restored.sql(calls, &[]).unwrap().rows, before);
    let error = restored
        .sql(
            "CREATE SERVER orphan_server FOREIGN DATA WRAPPER orphan_fdw",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert_eq!(
        error.to_string(),
        format!("cache lookup failed for function {old_oid}")
    );
    restored
        .sql(
            "CREATE SERVER live_server FOREIGN DATA WRAPPER live_fdw",
            &[],
        )
        .unwrap();
    let calls_for_server = restored
        .sql(
            "SELECT kind::integer AS kind FROM validator_calls WHERE kind = 1417::oid",
            &[],
        )
        .unwrap();
    assert_eq!(calls_for_server.rows.len(), 1);
    assert_eq!(calls_for_server.rows[0]["kind"], Value::Int(1417));
    assert_eq!(restored.sql(calls, &[]).unwrap().rows.len(), 2);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn large_callback_archive_yields_before_a_late_binding_failure_and_cancels_before_publication() {
    let files = tempfile::tempdir().unwrap();
    let source = Engine::new();
    source
        .sql(
            "CREATE FUNCTION live_validator(text[],oid) RETURNS integer LANGUAGE SQL RETURN 1",
            &[],
        )
        .unwrap();
    for index in 0..48 {
        source
            .sql(
                &format!("CREATE FOREIGN DATA WRAPPER w{index:03} VALIDATOR live_validator"),
                &[],
            )
            .unwrap();
    }
    let mut last = source.wrappers()["w047"].clone();
    last.validator.as_mut().unwrap().oid += 1;
    source
        .wrappers_write()
        .insert(last.name.clone(), last)
        .unwrap();
    let repository = repository(&files.path().join("repository"));
    let root = capture(&source, &repository, None);
    drop(source);
    for cancel in [false, true] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut job = begin(&repository, root, &control);
        while !matches!(
            &job.state,
            State::Validating(_, CatalogValidation::Relationships(_), ..)
        ) {
            job.advance(4096).unwrap();
        }
        for _ in 0..7 {
            assert_eq!(job.advance(4096).unwrap(), RestorePhase::Validating);
        }
        if cancel {
            control.cancellation().cancel();
        }
        let error = loop {
            match job.advance(4096) {
                Ok(RestorePhase::Validating) => (),
                Ok(state) => panic!("corrupt callback reached {state:?}"),
                Err(error) => break error,
            }
        };
        if cancel {
            assert!(matches!(error, BackupError::Cancelled(_)));
        } else {
            assert!(error.to_string().contains("w047"), "{error}");
        }
        assert!(matches!(job.publish(), Err(BackupError::Finished)));
        drop(job);
        assert_eq!(control.memory().used(), 0);
    }
}
