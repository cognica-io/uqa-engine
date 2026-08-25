//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hierarchy traversal and direct-leaf mutation regressions.

use super::exec;
use uqa_core::Value;
use uqa_engine::Engine;

#[test]
fn diamond_inheritance_scans_each_physical_descendant_once() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE hierarchy_root (id INTEGER)");
    exec(
        &engine,
        "CREATE TABLE hierarchy_left (left_value INTEGER) INHERITS (hierarchy_root)",
    );
    exec(
        &engine,
        "CREATE TABLE hierarchy_right (right_value INTEGER) INHERITS (hierarchy_root)",
    );
    exec(
        &engine,
        "CREATE TABLE hierarchy_diamond (leaf_value INTEGER) INHERITS (hierarchy_left, hierarchy_right)",
    );
    exec(
        &engine,
        "INSERT INTO hierarchy_diamond (id, left_value, right_value, leaf_value) VALUES (1, 2, 3, 4)",
    );

    let rows = engine.sql("SELECT id FROM hierarchy_root", &[]).unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0]["id"], Value::Int(1));
}

#[test]
fn point_update_cannot_bypass_a_direct_partition_bound() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE point_parent (id INTEGER PRIMARY KEY) PARTITION BY RANGE (id)",
    );
    exec(
        &engine,
        "CREATE TABLE point_leaf PARTITION OF point_parent FOR VALUES FROM (0) TO (10)",
    );
    exec(&engine, "INSERT INTO point_leaf VALUES (1)");

    let error = engine
        .sql("UPDATE point_leaf SET id = 20 WHERE id = 1", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23514"));
    assert_eq!(
        engine.sql("SELECT id FROM point_leaf", &[]).unwrap().rows[0]["id"],
        Value::Int(1)
    );
}

#[test]
fn truncate_hierarchy_honors_continue_and_restart_identity() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE truncate_parent (id SERIAL, bucket INTEGER) PARTITION BY RANGE (bucket)",
    );
    exec(
        &engine,
        "CREATE TABLE truncate_leaf PARTITION OF truncate_parent FOR VALUES FROM (0) TO (10)",
    );
    let first = engine
        .sql(
            "INSERT INTO truncate_parent (bucket) VALUES (1) RETURNING id",
            &[],
        )
        .unwrap();
    assert_eq!(first.rows[0]["id"], Value::Int(1));

    exec(&engine, "TRUNCATE truncate_parent CONTINUE IDENTITY");
    let continued = engine
        .sql(
            "INSERT INTO truncate_parent (bucket) VALUES (1) RETURNING id",
            &[],
        )
        .unwrap();
    assert_eq!(continued.rows[0]["id"], Value::Int(2));

    exec(&engine, "TRUNCATE truncate_parent RESTART IDENTITY");
    let restarted = engine
        .sql(
            "INSERT INTO truncate_parent (bucket) VALUES (1) RETURNING id",
            &[],
        )
        .unwrap();
    assert_eq!(restarted.rows[0]["id"], Value::Int(1));
}

#[test]
fn parent_for_update_locks_the_physical_partition_tuple() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("hierarchy-row-lock.db")).unwrap();
    exec(
        &root,
        "CREATE TABLE lock_parent (id INTEGER PRIMARY KEY) PARTITION BY RANGE (id)",
    );
    exec(
        &root,
        "CREATE TABLE lock_leaf PARTITION OF lock_parent FOR VALUES FROM (0) TO (10)",
    );
    exec(&root, "INSERT INTO lock_parent VALUES (1)");
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();

    exec(&holder, "BEGIN");
    holder
        .sql("SELECT id FROM lock_parent WHERE id = 1 FOR UPDATE", &[])
        .unwrap();
    let error = waiter
        .sql(
            "SELECT id FROM lock_leaf WHERE id = 1 FOR UPDATE NOWAIT",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("55P03"));
    exec(&holder, "ROLLBACK");
}
