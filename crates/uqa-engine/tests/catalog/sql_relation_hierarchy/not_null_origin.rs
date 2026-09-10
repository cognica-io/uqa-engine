//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable local origin of inherited NOT NULL constraints.

use super::{exec, Engine, Value};

fn assert_origin(engine: &Engine, table: &str, column: &str, name: &str, local: bool, count: i64) {
    let rows = engine.sql(&format!("SELECT x.conname, x.conislocal, x.coninhcount FROM pg_constraint x JOIN pg_attribute a ON a.attrelid=x.conrelid AND a.attnum=ANY(x.conkey) WHERE x.conrelid='{table}'::regclass AND x.contype='n' AND a.attname='{column}'"), &[]).unwrap().rows;
    assert_eq!(rows.len(), 1, "{table}.{column}");
    assert_eq!(
        rows[0]["conname"],
        Value::Str(name.into()),
        "{table}.{column}"
    );
    assert_eq!(
        rows[0]["conislocal"],
        Value::Bool(local),
        "{table}.{column}"
    );
    assert_eq!(
        rows[0]["coninhcount"],
        Value::Int(count),
        "{table}.{column}"
    );
}

#[test]
fn local_constraint_declarations_are_distinct_from_local_column_declarations_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("not-null-origin.db");
    {
        let engine = Engine::open(&path).unwrap();
        for sql in [
            "CREATE TABLE origin_parent(a integer CONSTRAINT parent_nn NOT NULL)",
            "CREATE TABLE origin_inherited() INHERITS(origin_parent)",
            "CREATE TABLE origin_redeclared(a integer) INHERITS(origin_parent)",
            "CREATE TABLE origin_local(a integer NOT NULL) INHERITS(origin_parent)",
            "CREATE TABLE origin_named(a integer CONSTRAINT child_nn NOT NULL) INHERITS(origin_parent)",
        ] { exec(&engine, sql); }
    }
    let engine = Engine::open(&path).unwrap();
    assert_origin(&engine, "origin_parent", "a", "parent_nn", true, 0);
    assert_origin(&engine, "origin_inherited", "a", "parent_nn", false, 1);
    assert_origin(&engine, "origin_redeclared", "a", "parent_nn", false, 1);
    assert_origin(
        &engine,
        "origin_local",
        "a",
        "origin_local_a_not_null",
        true,
        1,
    );
    assert_origin(&engine, "origin_named", "a", "child_nn", true, 1);
    exec(
        &engine,
        "ALTER TABLE origin_inherited ALTER COLUMN a SET NOT NULL",
    );
    assert_origin(&engine, "origin_inherited", "a", "parent_nn", true, 1);
}

#[test]
fn recursive_not_null_additions_preserve_local_origins_and_constraint_names() {
    for action in [
        "ALTER COLUMN a SET NOT NULL",
        "ADD CONSTRAINT requested_nn NOT NULL a",
    ] {
        let engine = Engine::new();
        for sql in [
            "CREATE TABLE origin_parent(a integer)",
            "CREATE TABLE origin_local(a integer CONSTRAINT retained_nn NOT NULL) INHERITS(origin_parent)",
            "CREATE TABLE origin_inherited() INHERITS(origin_parent)",
        ] { exec(&engine, sql); }
        exec(&engine, &format!("ALTER TABLE origin_parent {action}"));
        let parent_name = if action.starts_with("ADD") {
            "requested_nn"
        } else {
            "origin_parent_a_not_null"
        };
        assert_origin(&engine, "origin_parent", "a", parent_name, true, 0);
        assert_origin(&engine, "origin_local", "a", "retained_nn", true, 1);
        assert_origin(&engine, "origin_inherited", "a", parent_name, false, 1);
    }
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE origin_parent(a integer)",
        "CREATE TABLE origin_local(b integer CONSTRAINT retained_nn NOT NULL) INHERITS(origin_parent)",
        "CREATE TABLE origin_inherited() INHERITS(origin_parent)",
        "ALTER TABLE origin_parent ADD COLUMN b integer NOT NULL",
    ] { exec(&engine, sql); }
    assert_origin(&engine, "origin_local", "b", "retained_nn", true, 1);
    assert_origin(
        &engine,
        "origin_inherited",
        "b",
        "origin_parent_b_not_null",
        false,
        1,
    );
}

#[test]
fn parent_removal_and_partition_lifecycle_update_local_origin_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("not-null-origin-lifecycle.db");
    let engine = Engine::open(&path).unwrap();
    for sql in [
        "CREATE TABLE origin_left(a integer CONSTRAINT left_nn NOT NULL)",
        "CREATE TABLE origin_right(a integer CONSTRAINT right_nn NOT NULL)",
        "CREATE TABLE origin_shared() INHERITS(origin_left, origin_right)",
        "CREATE TABLE origin_partitioned(a integer CONSTRAINT partition_nn NOT NULL) PARTITION BY RANGE(a)",
        "CREATE TABLE origin_born PARTITION OF origin_partitioned FOR VALUES FROM(0) TO(10)",
        "CREATE TABLE origin_attached(a integer CONSTRAINT attached_nn NOT NULL)",
        "ALTER TABLE origin_partitioned ATTACH PARTITION origin_attached FOR VALUES FROM(10) TO(20)",
    ] { exec(&engine, sql); }
    assert_origin(&engine, "origin_shared", "a", "left_nn", false, 2);
    exec(&engine, "ALTER TABLE origin_shared NO INHERIT origin_right");
    assert_origin(&engine, "origin_shared", "a", "left_nn", false, 1);
    exec(&engine, "BEGIN");
    exec(&engine, "SAVEPOINT origin_change");
    exec(&engine, "ALTER TABLE origin_shared NO INHERIT origin_left");
    assert_origin(&engine, "origin_shared", "a", "left_nn", true, 0);
    exec(&engine, "ROLLBACK TO origin_change");
    assert_origin(&engine, "origin_shared", "a", "left_nn", false, 1);
    exec(&engine, "COMMIT");
    assert_origin(&engine, "origin_born", "a", "partition_nn", false, 1);
    assert_origin(&engine, "origin_attached", "a", "attached_nn", false, 1);
    exec(
        &engine,
        "ALTER TABLE origin_partitioned DETACH PARTITION origin_born",
    );
    drop(engine);
    let engine = Engine::open(&path).unwrap();
    assert_origin(&engine, "origin_born", "a", "partition_nn", true, 0);
    assert_origin(&engine, "origin_shared", "a", "left_nn", false, 1);
    assert_origin(&engine, "origin_attached", "a", "attached_nn", false, 1);
}

#[test]
fn becoming_local_and_validating_an_inherited_not_null_constraint_are_separate_changes() {
    let engine = Engine::new();
    for sql in [
        "CREATE TABLE origin_parent(a integer)",
        "CREATE TABLE origin_inherited() INHERITS(origin_parent)",
        "INSERT INTO origin_inherited VALUES(NULL)",
        "ALTER TABLE origin_parent ADD CONSTRAINT retained_nn NOT NULL a NOT VALID",
    ] {
        exec(&engine, sql);
    }
    assert_origin(&engine, "origin_inherited", "a", "retained_nn", false, 1);
    exec(
        &engine,
        "ALTER TABLE origin_inherited ALTER COLUMN a SET NOT NULL",
    );
    assert_origin(&engine, "origin_inherited", "a", "retained_nn", true, 1);
    assert_eq!(engine.sql("SELECT convalidated FROM pg_constraint WHERE conrelid='origin_inherited'::regclass", &[]).unwrap().rows[0]["convalidated"], Value::Bool(false));
    assert_eq!(
        engine
            .sql(
                "ALTER TABLE origin_inherited ALTER COLUMN a SET NOT NULL",
                &[]
            )
            .unwrap_err()
            .sqlstate(),
        Some("23502")
    );
    exec(&engine, "DELETE FROM origin_inherited");
    exec(
        &engine,
        "ALTER TABLE origin_inherited ALTER COLUMN a SET NOT NULL",
    );
    assert_eq!(engine.sql("SELECT convalidated FROM pg_constraint WHERE conrelid='origin_inherited'::regclass", &[]).unwrap().rows[0]["convalidated"], Value::Bool(true));
}

#[test]
fn no_inherit_constraints_are_not_copied_into_new_descendants() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE origin_parent(a integer CONSTRAINT local_nn NOT NULL NO INHERIT)",
    );
    exec(
        &engine,
        "CREATE TABLE origin_inherited() INHERITS(origin_parent)",
    );
    exec(&engine, "INSERT INTO origin_inherited VALUES(NULL)");
    assert!(engine
        .sql(
            "SELECT conname FROM pg_constraint WHERE conrelid='origin_inherited'::regclass",
            &[]
        )
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn serialized_origins_round_trip_and_legacy_columns_keep_their_local_projection() {
    let value =
        serde_json::json!({"name":"a", "ty":"Integer", "primary_key":false, "not_null":true});
    let mut column: uqa_sql::ast::ColumnDef = serde_json::from_value(value).unwrap();
    assert!(column.not_null_is_local);
    assert!(serde_json::to_value(&column)
        .unwrap()
        .get("not_null_is_local")
        .is_none());
    column.not_null_is_local = false;
    let serialized = serde_json::to_value(&column).unwrap();
    assert_eq!(serialized["not_null_is_local"], false);
    let restored: uqa_sql::ast::ColumnDef = serde_json::from_value(serialized).unwrap();
    assert!(!restored.not_null_is_local);
}
