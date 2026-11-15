//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{routine_bodies, *};
use crate::tests::providers::Provider;
use uqa_sql::ast::{Expr, FunctionBody};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_routine_bindings_reject_invalid_defaults_columns_and_results(
    #[case] provider: Provider,
) {
    for kind in ["default", "column", "result"] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let root = routine_bodies::capture_changed_definition(
            provider,
            &directory.path().join("source"),
            &repository,
            |definition| match kind {
                "default" => definition.params[0].default = Some(Expr::Literal(Value::Bool(true))),
                "column" => {
                    definition.body = FunctionBody::Statements(
                        uqa_sql::compile("SELECT missing_column").unwrap(),
                    );
                }
                "result" => {
                    definition.body =
                        FunctionBody::Statements(uqa_sql::compile("SELECT true").unwrap());
                }
                _ => unreachable!(),
            },
        );
        routine_bodies::reject(
            provider,
            &repository,
            root,
            &directory.path().join("restored"),
            match kind {
                "default" if !matches!(provider, Provider::Memory) => "routine-owned dependency bindings require an initial-open object-identity migration",
                "default" => "default",
                "column" => "missing_column",
                "result" => "return type",
                _ => unreachable!(),
            },
        );
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_routine_bindings_preserve_defaults_renames_typed_results_and_laziness(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&directory.path().join("source"));
    let (setup, observations) = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/backup_routine_bindings_oracle.sql"
    ))
    .split_once("-- capture boundary")
    .unwrap();
    let expected = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/backup_routine_bindings_oracle.expected.txt"
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
    let mut job = Engine::new()
        .begin_restore(
            &repository,
            root.backup(),
            super::providers::destination(provider, &path),
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
    let mut actual = String::new();
    for sql in observations.split(';').filter(|sql| !sql.trim().is_empty()) {
        let result = restored.sql(sql, &[]).unwrap();
        assert_eq!(result.rows.len(), 1, "{sql}");
        let value = result.rows[0].values().next().unwrap();
        match value {
            Value::Bool(value) => actual.push_str(if *value { "t" } else { "f" }),
            Value::Int(value) => actual.push_str(&value.to_string()),
            Value::Str(value) => actual.push_str(value),
            _ => panic!("unexpected fixture value: {value:?}"),
        }
        actual.push('\n');
    }
    assert_eq!(actual, expected);
    restored
        .sql("BEGIN; SELECT binding_update(8); ROLLBACK", &[])
        .unwrap();
    assert_eq!(
        restored
            .sql("SELECT label FROM binding_rows WHERE id=8", &[])
            .unwrap()
            .rows[0]["label"],
        Value::Str("eight".into())
    );
    drop(restored);
    assert_eq!(control.memory().used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path.join("database"));
        assert_eq!(
            reopened
                .sql("SELECT label FROM binding_read(7)", &[])
                .unwrap()
                .rows[0]["label"],
            Value::Str("seven!".into())
        );
        assert_eq!(
            reopened
                .sql("SELECT binding_divide(2) AS n", &[])
                .unwrap()
                .rows[0]["n"],
            Value::Int(5)
        );
    }
}
