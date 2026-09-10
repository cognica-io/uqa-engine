//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constant planning and prepared argument error ordering.

use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn constant_planning_matches_postgresql() {
    super::parameters::verify_parameters(
        &Engine::new(),
        include_str!("../../../../tests/parity/pg18/constant_planning_oracle.expected.json"),
    );
}

#[test]
fn prepared_plan_error_order_matches_postgresql() {
    super::parameters::verify_parameters(
        &Engine::new(),
        include_str!(
            "../../../../tests/parity/pg18/prepared_plan_error_order_oracle.expected.json"
        ),
    );
}

#[test]
fn rule_input_planning_matches_postgresql() {
    super::parameters::verify_parameters(
        &Engine::new(),
        include_str!("../../../../tests/parity/pg18/rule_input_planning_oracle.expected.json"),
    );
}

#[test]
fn rule_input_planning_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    super::parameters::verify_parameters(
        &Engine::open(&directory.path().join("rule-planning.db")).unwrap(),
        include_str!("../../../../tests/parity/pg18/rule_input_planning_oracle.expected.json"),
    );
}

fn assert_closed_empty_schema(engine: &Engine, table: &str) {
    for sql in [
        format!("SELECT missing, 1 / 0 FROM {table}"),
        format!("UPDATE {table} SET missing = 1 / 0"),
        format!("INSERT INTO {table} (missing) VALUES (1 / 0)"),
    ] {
        let error = engine.sql(&sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42703"), "{sql}: {error}");
    }
    engine
        .sql(&format!("INSERT INTO {table} DEFAULT VALUES"), &[])
        .unwrap();
}

#[test]
fn declared_empty_schemas_remain_closed_after_rollback_refresh_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("empty-schemas.db");
    let engine = Engine::open(&path).unwrap();
    let observer = Engine::open(&path).unwrap();
    engine.sql("CREATE TABLE empty_schema ()", &[]).unwrap();
    engine
        .sql("CREATE TABLE removed_schema (value integer)", &[])
        .unwrap();
    engine
        .sql("ALTER TABLE removed_schema DROP COLUMN value", &[])
        .unwrap();
    engine
        .create_default_table("open_documents", Vec::new())
        .unwrap();
    engine
        .add_document(
            "open_documents",
            1,
            std::collections::BTreeMap::from([("value".into(), Value::Int(7))]),
        )
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("ALTER TABLE empty_schema ADD COLUMN value integer", &[])
        .unwrap();
    engine.sql("ROLLBACK", &[]).unwrap();
    for table in ["empty_schema", "removed_schema"] {
        assert_closed_empty_schema(&engine, table);
        assert_closed_empty_schema(&observer, table);
    }
    drop(observer);
    drop(engine);
    let engine = Engine::open(&path).unwrap();
    for table in ["empty_schema", "removed_schema"] {
        assert_closed_empty_schema(&engine, table);
    }
    let result = engine.sql("SELECT value FROM open_documents", &[]).unwrap();
    assert_eq!(result.rows[0]["value"], Value::Int(7));
}

#[test]
fn planning_errors_preserve_random_state_and_coalesce_skips_runtime_arguments() {
    let engine = Engine::new();
    engine.sql("SELECT setseed(0.25)", &[]).unwrap();
    let expected = engine.sql("SELECT random() AS value", &[]).unwrap();
    engine.sql("SELECT setseed(0.25)", &[]).unwrap();
    engine
        .sql("SELECT CASE WHEN random() >= 0 THEN 1 / 0 ELSE 0 END", &[])
        .unwrap_err();
    let actual = engine.sql("SELECT random() AS value", &[]).unwrap();
    assert_eq!(actual.rows[0]["value"], expected.rows[0]["value"]);

    engine.sql("CREATE SEQUENCE coalesce_calls", &[]).unwrap();
    let result = engine
        .sql(
            "SELECT coalesce(NULL::bigint, nextval('coalesce_calls'), \
             nextval('coalesce_calls'), 1 / (random() * 0)::integer) AS value",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["value"], Value::Int(1));
    let state = engine
        .sql("SELECT currval('coalesce_calls') AS value", &[])
        .unwrap();
    assert_eq!(state.rows[0]["value"], Value::Int(1));
}
