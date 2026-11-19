//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn complete_repository_input_preserves_numeric_dynamic_and_shadowed_sequence_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source.sql("CREATE SEQUENCE ticket START 41; CREATE TABLE normal(value bigint DEFAULT nextval('ticket')); CREATE TABLE backup_numeric(value bigint DEFAULT nextval(2147483647::regclass)); CREATE VIEW backup_numeric_view AS SELECT currval(2147483647::regclass); CREATE TABLE backup_wrong_kind(value bigint DEFAULT nextval('backup_numeric'::regclass)); CREATE TABLE backup_dynamic(value bigint DEFAULT nextval('backup_absent'::text)); CREATE FUNCTION public.nextval(text) RETURNS bigint LANGUAGE SQL AS 'SELECT 17::bigint'; CREATE TABLE shadowed(value bigint DEFAULT public.nextval('no_relation')); SET check_function_bodies = off; CREATE FUNCTION lazy() RETURNS bigint LANGUAGE SQL AS 'SELECT currval(''absent_later'')'; SET check_function_bodies = on", &[]).unwrap();
    let expected = image(&source);
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let state = loop {
        if let Some(state) = input.advance(1024).unwrap() {
            break state;
        }
    };
    drop(input);
    let restored = state
        .into_private_engine(MemoryBudget::new(32 << 20), &control)
        .unwrap();
    assert_eq!(image(&restored), expected);
    // Independent PostgreSQL 18.4 outcomes are retained in backup_sequence_arguments_oracle.expected.txt.
    for (table, state) in [
        ("backup_numeric", "XX000"),
        ("backup_wrong_kind", "42809"),
        ("backup_dynamic", "42P01"),
    ] {
        assert_eq!(
            restored
                .sql(&format!("INSERT INTO {table} DEFAULT VALUES"), &[])
                .unwrap_err()
                .sqlstate(),
            Some(state),
            "{table}"
        );
    }
    restored
        .sql(
            "INSERT INTO normal DEFAULT VALUES; INSERT INTO shadowed DEFAULT VALUES",
            &[],
        )
        .unwrap();
    assert_eq!(
        restored.sql("SELECT value FROM normal", &[]).unwrap().rows[0]["value"],
        Value::Int(41)
    );
    assert_eq!(
        restored
            .sql("SELECT value FROM shadowed", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(17)
    );
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn complete_repository_input_rejects_missing_named_sequence_function_targets() {
    for missing in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = Engine::new();
        source
            .sql(
                "CREATE SEQUENCE ticket; CREATE TABLE items(id bigint DEFAULT nextval('ticket'))",
                &[],
            )
            .unwrap();
        {
            let tables = source.storage.tables.read();
            tables
                .get(&uqa_core::RelationIdentity::new("public", "items"))
                .unwrap()
                .columns
                .write()
                .edit(|columns| {
                    let uqa_sql::ast::Expr::Func { args, .. } =
                        columns[0].default.as_mut().unwrap()
                    else {
                        panic!("sequence default")
                    };
                    // Legacy stored declarations retain a canonical literal instead of a typed OID.
                    args[0] = uqa_sql::ast::Expr::Literal(Value::Str("public.ticket".into()));
                    Ok(())
                })
                .unwrap();
        }
        if missing {
            assert!(source
                .durable
                .sequences
                .write()
                .remove(&uqa_core::RelationIdentity::new("public", "ticket"))
                .unwrap());
        }
        let root = publish(&source, &repository);
        drop(source);
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&repository, root, &control);
        let state = loop {
            if let Some(state) = input.advance(1024).unwrap() {
                break state;
            }
        };
        drop(input);
        let restored = state.into_private_engine(MemoryBudget::new(32 << 20), &control);
        if missing {
            let Err(error) = restored else {
                panic!("missing named sequence function target reached private activation");
            };
            assert!(
                error.to_string().contains("native sequence function"),
                "{error}"
            );
        } else {
            let restored = restored.unwrap();
            restored
                .sql("INSERT INTO items DEFAULT VALUES", &[])
                .unwrap();
            assert_eq!(
                restored.sql("SELECT id FROM items", &[]).unwrap().rows[0]["id"],
                Value::Int(1)
            );
            drop(restored);
        }
        assert_eq!(control.memory().used(), 0);
    }
}
