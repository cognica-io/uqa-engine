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
            let root = capture_changed_definition(
                provider,
                &directory.path().join("source"),
                &repository,
                |definition| damage(definition, default, ordinal),
            );
            reject(
                provider,
                &repository,
                root,
                &directory.path().join("restored"),
                "parameter",
            );
        }
    }
}

pub(super) fn capture_changed_definition(
    provider: Provider,
    path: &std::path::Path,
    repository: &LocalRepository,
    damage: impl FnOnce(&mut CreateFunction),
) -> BackupRoot {
    let source = provider.open(path);
    source.sql("CREATE FUNCTION saved(n integer) RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT n; END", &[]).unwrap();
    if let Some(persistent) = source.storage.provider.clone() {
        drop(source);
        let session = persistent.open_session().unwrap();
        let key = "sql_functions_json";
        let mut record: serde_json::Value =
            serde_json::from_str(&session.catalog.get_metadata(key).unwrap().unwrap()).unwrap();
        let selected = &mut record["definitions"]["public.saved"][0];
        let mut definition: CreateFunction = serde_json::from_value(selected.clone()).unwrap();
        damage(&mut definition);
        *selected = serde_json::to_value(definition).unwrap();
        session
            .catalog
            .set_metadata(key, &record.to_string())
            .unwrap();
        drop(session);
        capture_provider(persistent.as_ref(), repository)
    } else {
        {
            let mut routines = source.durable.sql_user_functions.write();
            let original = &routines["public.saved"][0];
            let mut definition = original.def.clone();
            damage(&mut definition);
            let changed = SQLUserFunction::new(definition, original.body.clone()).shared();
            routines
                .insert("public.saved".into(), vec![changed])
                .unwrap();
        }
        let root = capture(&source, repository, None);
        drop(source);
        root
    }
}

pub(super) fn reject(
    provider: Provider,
    repository: &LocalRepository,
    root: BackupRoot,
    path: &std::path::Path,
    expected: &str,
) {
    reject_with_window(provider, repository, root, path, expected, 1024);
}

fn reject_with_window(
    provider: Provider,
    repository: &LocalRepository,
    root: BackupRoot,
    path: &std::path::Path,
    expected: &str,
    validation_steps: usize,
) -> usize {
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
    let mut windows = 0;
    let error = loop {
        let steps = if job.phase() == RestorePhase::Validating {
            windows += 1;
            validation_steps
        } else {
            1024
        };
        match job.advance(steps) {
            Err(error) => break error,
            Ok(RestorePhase::Reading | RestorePhase::Validating) => (),
            Ok(phase) => panic!("invalid routine reached {phase:?}"),
        }
    };
    assert!(
        error.to_string().contains(expected),
        "{provider:?}: {error}"
    );
    drop(error);
    assert_eq!(job.phase(), RestorePhase::Failed);
    assert!(!path.exists());
    assert_eq!(control.memory().used(), 0);
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
    drop(job);
    repository.remove_backup(root.backup(), &control).unwrap();
    assert_eq!(control.memory().used(), 0);
    windows
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

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_routine_bodies_reject_missing_original_relations_with_valid_retained_plans(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let root = capture_changed_definition(
        provider,
        &directory.path().join("source"),
        &repository,
        |definition| {
            definition.body = FunctionBody::Statements(
                uqa_sql::compile("SELECT id FROM public.missing").unwrap(),
            );
        },
    );
    reject(
        provider,
        &repository,
        root,
        &directory.path().join("restored"),
        "missing",
    );
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_grouped_routine_reports_the_first_ungrouped_column_in_its_own_work_window(
    #[case] provider: Provider,
) {
    const WIDTH: usize = 32;
    let run = |invalid| {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let columns = (0..WIDTH)
            .map(|i| if i == invalid { "x" } else { "v" })
            .collect::<Vec<_>>()
            .join(",");
        let root = capture_changed_definition(
            provider,
            &directory.path().join("source"),
            &repository,
            |definition| {
                let sql = format!("SELECT {columns} FROM (VALUES (1,2)) AS t(v,x) GROUP BY DISTINCT GROUPING SETS ((v),(v))");
                definition.body = FunctionBody::Statements(uqa_sql::compile(&sql).unwrap());
            },
        );
        reject_with_window(
            provider,
            &repository,
            root,
            &directory.path().join("restored"),
            r#"column "t.x" must appear in the GROUP BY clause or be used in an aggregate function"#,
            1,
        )
    };
    let first = run(0);
    let last = run(WIDTH - 1);
    println!("{provider:?} grouped projection validation windows: {first} first, {last} last");
    assert!(
        last >= first + WIDTH - 1,
        "{provider:?}: {first} first vs {last} last"
    );
}
