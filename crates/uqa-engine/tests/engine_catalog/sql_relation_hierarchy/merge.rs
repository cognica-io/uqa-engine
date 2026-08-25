//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE execution over inherited and partitioned targets.

use super::{exec, Engine, Value};

fn assert_merge_partition_rows(engine: &Engine) {
    let low = engine
        .sql(
            "SELECT id, bucket, value FROM merge_targets_low ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(low.rows.len(), 1);
    assert_eq!(low.rows[0]["id"], Value::Int(1));
    assert_eq!(low.rows[0]["value"], Value::Str("low-updated".into()));
    let high = engine
        .sql(
            "SELECT id, bucket, value FROM merge_targets_high ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(high.rows.len(), 3);
    assert_eq!(high.rows[0]["id"], Value::Int(1));
    assert_eq!(high.rows[0]["value"], Value::Str("high-updated".into()));
    assert_eq!(high.rows[1]["id"], Value::Int(2));
    assert_eq!(high.rows[1]["bucket"], Value::Int(12));
    assert_eq!(high.rows[2]["id"], Value::Int(4));
}

#[test]
fn merge_tracks_physical_partition_identity_and_routes_actions_under_spill() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE merge_targets (id INTEGER, bucket INTEGER, value TEXT) PARTITION BY RANGE (bucket)",
    );
    exec(
        &engine,
        "CREATE TABLE merge_targets_low PARTITION OF merge_targets FOR VALUES FROM (0) TO (10)",
    );
    exec(
        &engine,
        "CREATE TABLE merge_targets_high PARTITION OF merge_targets FOR VALUES FROM (10) TO (20)",
    );
    exec(
        &engine,
        "INSERT INTO merge_targets VALUES (1, 1, 'low'), (1, 11, 'high'), (2, 2, 'move'), (3, 3, 'delete')",
    );
    exec(
        &engine,
        "CREATE TABLE merge_source (source_seq INTEGER PRIMARY KEY, id INTEGER, old_bucket INTEGER, new_bucket INTEGER, value TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO merge_source VALUES (1, 1, 1, 1, 'low-updated'), (2, 1, 11, 11, 'high-updated'), (3, 2, 2, 12, 'moved'), (4, 4, 14, 14, 'inserted')",
    );
    exec(&engine, "SET work_mem TO '1B'");

    let returned = engine
        .sql(
            "MERGE INTO merge_targets AS target USING merge_source AS source ON target.id = source.id AND target.bucket = source.old_bucket WHEN MATCHED THEN UPDATE SET bucket = source.new_bucket, value = source.value WHEN NOT MATCHED BY SOURCE THEN DELETE WHEN NOT MATCHED THEN INSERT (id, bucket, value) VALUES (source.id, source.new_bucket, source.value) RETURNING merge_action() AS action, old.id AS old_id, old.bucket AS old_bucket, new.id AS new_id, new.bucket AS new_bucket, new.value AS new_value",
            &[],
        )
        .unwrap();
    assert_eq!(returned.rows.len(), 5);
    assert_eq!(
        returned
            .rows
            .iter()
            .filter(|row| row["action"] == Value::Str("UPDATE".into()))
            .count(),
        3
    );
    assert_eq!(
        returned
            .rows
            .iter()
            .filter(|row| row["action"] == Value::Str("DELETE".into()))
            .count(),
        1
    );
    assert_eq!(
        returned
            .rows
            .iter()
            .filter(|row| row["action"] == Value::Str("INSERT".into()))
            .count(),
        1
    );
    let moved = returned
        .rows
        .iter()
        .find(|row| row["old_id"] == Value::Int(2))
        .unwrap();
    assert_eq!(moved["old_bucket"], Value::Int(2));
    assert_eq!(moved["new_bucket"], Value::Int(12));

    assert_merge_partition_rows(&engine);

    exec(
        &engine,
        "TRUNCATE merge_source; INSERT INTO merge_source VALUES (9, 9, 5, 5, 'only-insert')",
    );
    let only = engine
        .sql(
            "MERGE INTO ONLY merge_targets AS target USING merge_source AS source ON target.id = source.id AND target.bucket = source.old_bucket WHEN NOT MATCHED THEN INSERT (id, bucket, value) VALUES (source.id, source.new_bucket, source.value) RETURNING new.id AS id, new.value AS value",
            &[],
        )
        .unwrap();
    assert_eq!(only.rows.len(), 1);
    assert_eq!(only.rows[0]["id"], Value::Int(9));
    assert_eq!(only.rows[0]["value"], Value::Str("only-insert".into()));
    assert_eq!(
        engine
            .sql("SELECT value FROM merge_targets_low WHERE id = 9", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Str("only-insert".into())
    );
}

#[test]
fn merge_only_limits_ordinary_inheritance_matching_but_not_parent_inserts() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE merge_parent (id INTEGER, value TEXT)",
    );
    exec(
        &engine,
        "CREATE TABLE merge_child (extra TEXT) INHERITS (merge_parent)",
    );
    exec(
        &engine,
        "INSERT INTO merge_parent VALUES (1, 'parent'); INSERT INTO merge_child VALUES (2, 'child', 'extra')",
    );
    exec(
        &engine,
        "CREATE TABLE merge_only_source (id INTEGER, value TEXT); INSERT INTO merge_only_source VALUES (1, 'parent-updated'), (2, 'inserted-parent')",
    );

    let returned = engine
        .sql(
            "MERGE INTO ONLY merge_parent AS target USING merge_only_source AS source ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = source.value WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value) RETURNING merge_action() AS action, new.id AS id, new.value AS value",
            &[],
        )
        .unwrap();
    assert_eq!(returned.rows.len(), 2);
    let parent = engine
        .sql("SELECT id, value FROM ONLY merge_parent ORDER BY id", &[])
        .unwrap();
    assert_eq!(parent.rows.len(), 2);
    assert_eq!(parent.rows[0]["value"], Value::Str("parent-updated".into()));
    assert_eq!(
        parent.rows[1]["value"],
        Value::Str("inserted-parent".into())
    );
    let child = engine
        .sql("SELECT id, value FROM ONLY merge_child", &[])
        .unwrap();
    assert_eq!(child.rows.len(), 1);
    assert_eq!(child.rows[0]["id"], Value::Int(2));
    assert_eq!(child.rows[0]["value"], Value::Str("child".into()));
}

#[test]
fn merge_scans_a_multiply_inherited_descendant_once() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE merge_root (id INTEGER, value TEXT); CREATE TABLE merge_left () INHERITS (merge_root); CREATE TABLE merge_right () INHERITS (merge_root); CREATE TABLE merge_diamond () INHERITS (merge_left, merge_right)",
    );
    exec(
        &engine,
        "INSERT INTO merge_diamond VALUES (7, 'before'); CREATE TABLE merge_diamond_source (id INTEGER, value TEXT); INSERT INTO merge_diamond_source VALUES (7, 'after')",
    );
    let returned = engine
        .sql(
            "MERGE INTO merge_root AS target USING merge_diamond_source AS source ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = source.value RETURNING new.id AS id, new.value AS value",
            &[],
        )
        .unwrap();
    assert_eq!(returned.rows.len(), 1);
    assert_eq!(returned.rows[0]["id"], Value::Int(7));
    assert_eq!(returned.rows[0]["value"], Value::Str("after".into()));
    let stored = engine
        .sql("SELECT id, value FROM ONLY merge_diamond", &[])
        .unwrap();
    assert_eq!(stored.rows.len(), 1);
    assert_eq!(stored.rows[0]["value"], Value::Str("after".into()));
}
