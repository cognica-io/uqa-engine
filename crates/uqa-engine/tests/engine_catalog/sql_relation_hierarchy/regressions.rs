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
