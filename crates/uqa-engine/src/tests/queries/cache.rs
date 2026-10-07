//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::SQLParam;

#[rstest::rstest]
#[case::memory(None)]
#[case::sqlite(Some(0))]
#[case::sqlite_kv(Some(1))]
#[case::redb(Some(2))]
fn parsed_mutations_survive_data_changes_but_preserve_rollback_and_schema_checks(
    #[case] provider: Option<usize>,
) {
    let persistent = provider.map(crate::tests::relation_lock_support::sessions);
    let memory = Engine::new();
    let engine = persistent.as_ref().map_or(&memory, |(_, engine, _)| engine);
    engine
        .sql("CREATE TABLE cached_inserts(id integer PRIMARY KEY)", &[])
        .unwrap();
    let insert = "INSERT INTO cached_inserts(id) VALUES ($1) RETURNING id";
    engine
        .sql(insert, &[SQLParam::Scalar(Value::Int(1))])
        .unwrap();
    let parsed = engine.cached_sql_statement(insert).unwrap().statement;
    for value in 2..=4 {
        let result = engine
            .sql(insert, &[SQLParam::Scalar(Value::Int(value))])
            .unwrap();
        assert_eq!(result.rows[0]["id"], Value::Int(value));
        assert!(Arc::ptr_eq(
            &parsed,
            &engine.cached_sql_statement(insert).unwrap().statement
        ));
    }
    engine.begin().unwrap();
    engine
        .sql(insert, &[SQLParam::Scalar(Value::Int(5))])
        .unwrap();
    engine.rollback().unwrap();
    assert_eq!(
        engine
            .sql(insert, &[SQLParam::Scalar(Value::Int(3))])
            .unwrap_err()
            .sqlstate(),
        Some("23505")
    );
    engine
        .sql(insert, &[SQLParam::Scalar(Value::Int(6))])
        .unwrap();
    let rows = engine
        .sql("SELECT id FROM cached_inserts ORDER BY id", &[])
        .unwrap()
        .rows;
    assert_eq!(
        rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>(),
        [1, 2, 3, 4, 6].map(Value::Int)
    );
    engine
        .sql(
            "ALTER TABLE cached_inserts RENAME COLUMN id TO renamed",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql(insert, &[SQLParam::Scalar(Value::Int(7))])
            .unwrap_err()
            .sqlstate(),
        Some("42703")
    );
    assert_eq!(
        engine
            .sql("SELECT id FROM cached_inserts ORDER BY id", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42703")
    );
}
