//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! NOT NULL inheritance boundaries and durable constraint identity.

use super::{exec, Engine, Value};

fn constraint(engine: &Engine, table: &str) -> Vec<std::collections::BTreeMap<String, Value>> {
    engine.sql(&format!("SELECT oid, conname, connoinherit, convalidated FROM pg_constraint WHERE conrelid = '{table}'::regclass AND contype = 'n' ORDER BY conname"), &[]).unwrap().rows
}

fn error(engine: &Engine, sql: &str, state: &str, message: &str) {
    let error = engine.sql(sql, &[]).expect_err(sql);
    assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
    assert_eq!(error.to_string(), message, "{sql}");
}

#[test]
fn only_not_null_preserves_no_inherit_identity_through_rollback_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("only-not-null.db");
    let saved;
    {
        let engine = Engine::open(&path).unwrap();
        exec(&engine, "CREATE TABLE nn_parent(a integer)");
        exec(&engine, "CREATE TABLE nn_child() INHERITS(nn_parent)");
        exec(&engine, "BEGIN");
        exec(&engine, "SAVEPOINT before_constraint");
        exec(
            &engine,
            "ALTER TABLE ONLY nn_parent ALTER COLUMN a SET NOT NULL",
        );
        assert_eq!(
            constraint(&engine, "nn_parent")[0]["connoinherit"],
            Value::Bool(true)
        );
        exec(&engine, "ROLLBACK TO before_constraint");
        assert!(constraint(&engine, "nn_parent").is_empty());
        exec(
            &engine,
            "ALTER TABLE ONLY nn_parent ALTER COLUMN a SET NOT NULL",
        );
        exec(&engine, "COMMIT");
        saved = constraint(&engine, "nn_parent");
        exec(
            &engine,
            "ALTER TABLE ONLY nn_parent ALTER COLUMN a SET NOT NULL",
        );
        assert_eq!(constraint(&engine, "nn_parent"), saved);
        exec(&engine, "INSERT INTO nn_child VALUES (NULL)");
    }
    let engine = Engine::open(&path).unwrap();
    assert_eq!(constraint(&engine, "nn_parent"), saved);
    error(&engine, "ALTER TABLE nn_parent ALTER COLUMN a SET NOT NULL", "0A000",
        "cannot change NO INHERIT status of NOT NULL constraint \"nn_parent_a_not_null\" on relation \"nn_parent\"");
    assert_eq!(constraint(&engine, "nn_parent"), saved);
    assert!(constraint(&engine, "nn_child").is_empty());
    exec(
        &engine,
        "ALTER TABLE ONLY nn_parent ALTER COLUMN a DROP NOT NULL",
    );
    assert_eq!(
        engine
            .sql("ALTER TABLE nn_parent ALTER COLUMN a SET NOT NULL", &[])
            .unwrap_err()
            .sqlstate(),
        Some("23502")
    );
    assert!(constraint(&engine, "nn_parent").is_empty());
    assert!(constraint(&engine, "nn_child").is_empty());
    exec(&engine, "DELETE FROM nn_child");
    exec(&engine, "ALTER TABLE nn_parent ALTER COLUMN a SET NOT NULL");
    assert_eq!(
        constraint(&engine, "nn_parent")[0]["connoinherit"],
        Value::Bool(false)
    );
    assert_eq!(
        engine
            .sql("INSERT INTO nn_child VALUES(NULL)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("23502")
    );
}

#[test]
fn only_not_null_distinguishes_leaf_and_partition_parents() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE nn_leaf(a integer)",
        "ALTER TABLE ONLY nn_leaf ALTER COLUMN a SET NOT NULL",
        "CREATE TABLE nn_partitioned(a integer) PARTITION BY RANGE(a)",
        "CREATE TABLE nn_partition PARTITION OF nn_partitioned FOR VALUES FROM(0) TO(10)",
        "CREATE TABLE nn_empty_partitioned(a integer) PARTITION BY RANGE(a)",
        "ALTER TABLE ONLY nn_empty_partitioned ALTER COLUMN a SET NOT NULL",
    ] {
        exec(&engine, sql);
    }
    for table in ["nn_leaf", "nn_empty_partitioned"] {
        assert_eq!(
            constraint(&engine, table)[0]["connoinherit"],
            Value::Bool(false)
        );
        exec(
            &engine,
            &format!("ALTER TABLE {table} ALTER COLUMN a SET NOT NULL"),
        );
    }
    error(
        &engine,
        "ALTER TABLE ONLY nn_partitioned ALTER COLUMN a SET NOT NULL",
        "42P16",
        "constraint must be added to child tables too",
    );
    assert!(constraint(&engine, "nn_partitioned").is_empty());
    assert!(constraint(&engine, "nn_partition").is_empty());
    exec(
        &engine,
        "ALTER TABLE nn_partitioned ALTER COLUMN a SET NOT NULL",
    );
    let before = constraint(&engine, "nn_partitioned");
    exec(
        &engine,
        "ALTER TABLE ONLY nn_partitioned ALTER COLUMN a SET NOT NULL",
    );
    assert_eq!(constraint(&engine, "nn_partitioned"), before);
}

#[test]
fn recursive_not_null_rejects_existing_no_inherit_before_mutation() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE nn_parent(a integer)");
    exec(&engine, "CREATE TABLE nn_child(a integer CONSTRAINT child_nn NOT NULL NO INHERIT) INHERITS(nn_parent)");
    let before = constraint(&engine, "nn_child");
    for (action, state) in [
        ("ALTER COLUMN a SET NOT NULL", "0A000"),
        ("ADD CONSTRAINT parent_nn NOT NULL a", "55000"),
    ] {
        error(&engine, &format!("ALTER TABLE nn_parent {action}"), state,
            "cannot change NO INHERIT status of NOT NULL constraint \"child_nn\" on relation \"nn_child\"");
        assert!(constraint(&engine, "nn_parent").is_empty());
        assert_eq!(constraint(&engine, "nn_child"), before);
    }
    exec(&engine, "CREATE ROLE nn_owner");
    exec(&engine, "ALTER TABLE nn_parent OWNER TO nn_owner");
    exec(&engine, "GRANT ALL ON nn_child TO nn_owner");
    exec(&engine, "SET ROLE nn_owner");
    error(
        &engine,
        "ALTER TABLE nn_parent ALTER COLUMN a SET NOT NULL",
        "42501",
        "must be owner of table nn_child",
    );
    exec(&engine, "RESET ROLE");
    assert!(constraint(&engine, "nn_parent").is_empty());
    assert_eq!(constraint(&engine, "nn_child"), before);
}

#[test]
fn explicit_no_inherit_and_unvalidated_not_null_keep_their_names() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nn_explicit(a integer CONSTRAINT chosen_nn NOT NULL NO INHERIT)",
    );
    let before = constraint(&engine, "nn_explicit");
    error(&engine, "ALTER TABLE nn_explicit ALTER COLUMN a SET NOT NULL", "0A000",
        "cannot change NO INHERIT status of NOT NULL constraint \"chosen_nn\" on relation \"nn_explicit\"");
    exec(
        &engine,
        "ALTER TABLE ONLY nn_explicit ALTER COLUMN a SET NOT NULL",
    );
    assert_eq!(constraint(&engine, "nn_explicit"), before);
    exec(&engine, "CREATE TABLE nn_unvalidated(a integer)");
    exec(
        &engine,
        "ALTER TABLE nn_unvalidated ADD CONSTRAINT retained_nn NOT NULL a NOT VALID",
    );
    let before = constraint(&engine, "nn_unvalidated");
    exec(
        &engine,
        "ALTER TABLE nn_unvalidated ALTER COLUMN a SET NOT NULL",
    );
    let after = constraint(&engine, "nn_unvalidated");
    assert_eq!(after[0]["oid"], before[0]["oid"]);
    assert_eq!(after[0]["conname"], Value::Str("retained_nn".into()));
    assert_eq!(after[0]["convalidated"], Value::Bool(true));
}

#[test]
fn set_not_null_reports_column_and_existing_row_errors_atomically() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE nn_errors(a integer)");
    error(
        &engine,
        "ALTER TABLE nn_errors ALTER COLUMN absent SET NOT NULL",
        "42703",
        "column \"absent\" of relation \"nn_errors\" does not exist",
    );
    error(
        &engine,
        "ALTER TABLE nn_errors ALTER COLUMN xmin SET NOT NULL",
        "0A000",
        "cannot alter system column \"xmin\"",
    );
    exec(&engine, "INSERT INTO nn_errors VALUES(NULL)");
    error(
        &engine,
        "ALTER TABLE nn_errors ALTER COLUMN a SET NOT NULL",
        "23502",
        "column \"a\" of relation \"nn_errors\" contains null values",
    );
    assert!(constraint(&engine, "nn_errors").is_empty());
}
