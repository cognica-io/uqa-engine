//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::providers::{capture_provider, destination};
use super::*;
use crate::tests::providers::Provider;
use uqa_sql::{
    ast::{CreateFunction, Expr, FunctionBody, Statement},
    routines::{RoutineBody, SQLUserFunction},
};

fn damage(definition: &mut CreateFunction, default: bool, ordinal: usize) {
    if default {
        definition.params[0].default = Some(Expr::Param(ordinal));
    } else {
        definition.body = FunctionBody::Statements(vec![Statement::Values {
            rows: vec![vec![Expr::Param(ordinal)]],
        }]);
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_routine_bodies_reject_parameters_outside_their_declared_scope(
    #[case] provider: Provider,
) {
    for default in [false, true] {
        for ordinal in [0, 2] {
            let directory = tempfile::tempdir().unwrap();
            let repository = repository(&directory.path().join("repository"));
            let source = provider.open(&directory.path().join("source"));
            source.sql("CREATE FUNCTION saved(n integer) RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT n; END", &[]).unwrap();
            let root = if let Some(persistent) = source.storage.provider.clone() {
                drop(source);
                let session = persistent.open_session().unwrap();
                let key = "sql_functions_json";
                let mut record: serde_json::Value =
                    serde_json::from_str(&session.catalog.get_metadata(key).unwrap().unwrap())
                        .unwrap();
                let selected = &mut record["definitions"]["public.saved"][0];
                let mut definition: CreateFunction =
                    serde_json::from_value(selected.clone()).unwrap();
                damage(&mut definition, default, ordinal);
                *selected = serde_json::to_value(definition).unwrap();
                session
                    .catalog
                    .set_metadata(key, &record.to_string())
                    .unwrap();
                drop(session);
                capture_provider(persistent.as_ref(), &repository)
            } else {
                {
                    let mut routines = source.durable.sql_user_functions.write();
                    let original = &routines["public.saved"][0];
                    let mut definition = original.def.clone();
                    damage(&mut definition, default, ordinal);
                    let changed = SQLUserFunction::new(definition, original.body.clone()).shared();
                    routines
                        .insert("public.saved".into(), vec![changed])
                        .unwrap();
                }
                let root = capture(&source, &repository, None);
                drop(source);
                root
            };
            reject(
                provider,
                &repository,
                root,
                &directory.path().join("restored"),
            );
        }
    }
}

fn reject(
    provider: Provider,
    repository: &LocalRepository,
    root: BackupRoot,
    path: &std::path::Path,
) {
    let control = StorageReadControl::with_limit(32 << 20);
    let authority = Engine::new();
    let mut job = authority
        .begin_restore(
            repository,
            root.backup(),
            destination(provider, path),
            RestoreOptions::new(16 << 20, 8 << 20),
            &control,
        )
        .unwrap();
    let error = loop {
        match job.advance(1024) {
            Err(error) => break error,
            Ok(RestorePhase::Reading | RestorePhase::Validating) => (),
            Ok(phase) => panic!("invalid routine reached {phase:?}"),
        }
    };
    assert!(
        error.to_string().contains("parameter"),
        "{provider:?}: {error}"
    );
    assert_eq!(job.phase(), RestorePhase::Failed);
    assert!(!path.exists());
    assert_eq!(control.memory().used(), 0);
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
    drop(job);
    repository.remove_backup(root.backup(), &control).unwrap();
    assert_eq!(control.memory().used(), 0);
}

fn observations(engine: &Engine) -> String {
    let value = engine
        .sql("SELECT record_input(4) AS value", &[])
        .unwrap()
        .rows[0]["value"]
        .clone();
    let output = engine.sql("CALL add_inputs(2,NULL,3)", &[]).unwrap().rows[0]["result"].clone();
    let recorded = engine
        .sql(
            "SELECT string_agg(value::text,',' ORDER BY value) AS recorded FROM routine_log",
            &[],
        )
        .unwrap()
        .rows[0]["recorded"]
        .clone();
    let state = engine
        .sql("SELECT lazy_state() AS state", &[])
        .unwrap()
        .rows[0]["state"]
        .clone();
    let (Value::Int(value), Value::Int(output), Value::Str(recorded), Value::Str(state)) =
        (value, output, recorded, state)
    else {
        panic!("routine outputs")
    };
    format!("{value}\n{output}\n{recorded}\n{state}\n")
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_routine_bodies_preserve_input_positions_effects_and_lazy_compilation(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&directory.path().join("source"));
    let (setup, _) = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/backup_routine_parameters_oracle.sql"
    ))
    .split_once("-- capture boundary")
    .unwrap();
    let expected = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/backup_routine_parameters_oracle.expected.txt"
    ));
    source.sql(setup, &[]).unwrap();
    let definitions = source
        .durable
        .sql_user_functions
        .read()
        .iter()
        .flat_map(|(_, entries)| {
            entries
                .iter()
                .map(|routine| serde_json::to_value(&routine.def).unwrap())
        })
        .collect::<Vec<_>>();
    let root = capture(&source, &repository, None);
    drop(source);
    let path = directory.path().join("restored");
    let control = StorageReadControl::with_limit(32 << 20);
    let authority = Engine::new();
    let mut job = authority
        .begin_restore(
            &repository,
            root.backup(),
            destination(provider, &path),
            RestoreOptions::new(16 << 20, 8 << 20),
            &control,
        )
        .unwrap();
    prepare(&mut job);
    let restored = job.publish().unwrap();
    drop(job);
    repository
        .remove_backup(root.backup(), &StorageReadControl::with_limit(32 << 20))
        .unwrap();
    assert!(restored
        .sql("SELECT * FROM routine_log", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        restored
            .durable
            .sql_user_functions
            .read()
            .iter()
            .flat_map(|(_, entries)| entries
                .iter()
                .map(|routine| serde_json::to_value(&routine.def).unwrap()))
            .collect::<Vec<_>>(),
        definitions
    );
    assert!(matches!(
        restored.durable.sql_user_functions.read()["public.lazy_missing"][0].body,
        RoutineBody::Source
    ));
    assert_eq!(observations(&restored), expected);
    restored
        .sql("BEGIN; SELECT record_input(99); ROLLBACK", &[])
        .unwrap();
    assert_eq!(
        restored
            .sql("SELECT count(*) AS n FROM routine_log", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(1)
    );
    drop(restored);
    assert_eq!(control.memory().used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path.join("database"));
        reopened.sql("TRUNCATE routine_log", &[]).unwrap();
        assert_eq!(observations(&reopened), expected);
    }
}
