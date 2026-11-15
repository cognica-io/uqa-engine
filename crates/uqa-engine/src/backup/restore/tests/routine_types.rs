//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::providers::destination;
use super::routine_bodies::{capture_changed_definition, reject};
use super::*;
use crate::tests::providers::Provider;
use uqa_sql::ast::{Expr, FunctionBody, Statement};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_routine_types_reject_missing_default_and_original_body_references(
    #[case] provider: Provider,
) {
    for default in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let root = capture_changed_definition(
            provider,
            &directory.path().join("source"),
            &repository,
            |definition| {
                let expression = Expr::Cast {
                    implicit: false,
                    expr: Box::new(Expr::Literal(Value::Null)),
                    ty: "enum#999999".into(),
                };
                if default {
                    definition.params[0].default = Some(expression);
                } else {
                    definition.body = FunctionBody::Statements(vec![Statement::Values {
                        rows: vec![vec![expression]],
                    }]);
                }
            },
        );
        reject(
            provider,
            &repository,
            root,
            &directory.path().join("restored"),
            "missing type",
        );
    }
}

fn observations(engine: &Engine) -> String {
    let mut output = String::new();
    for sql in ["SELECT mood_read() AS v", "SELECT mood_read('busy') AS v"] {
        let result = engine.sql(sql, &[]).unwrap();
        let Value::Str(value) = &result.rows[0]["v"] else {
            panic!("text result")
        };
        output.push_str(value);
        output.push('\n');
    }
    output
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_routine_types_preserve_selected_identity_after_name_replacement(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&directory.path().join("source"));
    let (setup, _) = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/backup_routine_types_oracle.sql"
    ))
    .split_once("-- capture boundary")
    .unwrap();
    let expected = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/backup_routine_types_oracle.expected.txt"
    ));
    source.sql(setup, &[]).unwrap();
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
    repository.remove_backup(root.backup(), &control).unwrap();
    assert_eq!(observations(&restored), expected);
    restored
        .sql(
            "BEGIN; ALTER TYPE retained_mood RENAME TO temporary_mood; ROLLBACK",
            &[],
        )
        .unwrap();
    assert_eq!(observations(&restored), expected);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
    if !matches!(provider, Provider::Memory) {
        assert_eq!(
            observations(&provider.open(&path.join("database"))),
            expected
        );
    }
}
