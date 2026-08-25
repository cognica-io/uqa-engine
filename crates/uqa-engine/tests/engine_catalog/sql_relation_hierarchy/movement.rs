//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cross-partition UPDATE routing, physical identity, and statement atomicity.

use super::{exec, Engine, Value};
use std::sync::mpsc;
use std::time::Duration;

fn create_range_movement_fixture(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE movement_targets (item_key INTEGER, bucket INTEGER, value TEXT) PARTITION BY RANGE (bucket)",
    );
    exec(
        engine,
        "CREATE TABLE movement_targets_low PARTITION OF movement_targets FOR VALUES FROM (0) TO (10)",
    );
    exec(
        engine,
        "CREATE TABLE movement_targets_high PARTITION OF movement_targets FOR VALUES FROM (10) TO (20)",
    );
}

#[test]
fn parent_update_moves_a_physical_row_and_returning_uses_both_row_images() {
    let engine = Engine::new();
    create_range_movement_fixture(&engine);
    exec(
        &engine,
        "INSERT INTO movement_targets VALUES (1, 1, 'low'), (2, 11, 'high')",
    );
    let low_doc_id = engine
        .sql("SELECT _doc_id FROM movement_targets_low", &[])
        .unwrap()
        .rows[0]["_doc_id"]
        .clone();
    let high_doc_id = engine
        .sql("SELECT _doc_id FROM movement_targets_high", &[])
        .unwrap()
        .rows[0]["_doc_id"]
        .clone();
    assert_eq!(low_doc_id, high_doc_id);

    let returned = engine
        .sql(
            "UPDATE movement_targets SET bucket = 12, value = 'moved' WHERE item_key = 1 RETURNING old._doc_id AS old_doc_id, new._doc_id AS new_doc_id, old.bucket AS old_bucket, new.bucket AS new_bucket, old.value AS old_value, new.value AS new_value",
            &[],
        )
        .unwrap();
    assert_eq!(returned.rows.len(), 1);
    assert_eq!(returned.rows[0]["old_doc_id"], low_doc_id);
    assert_ne!(returned.rows[0]["new_doc_id"], high_doc_id);
    assert_eq!(returned.rows[0]["old_bucket"], Value::Int(1));
    assert_eq!(returned.rows[0]["new_bucket"], Value::Int(12));
    assert_eq!(returned.rows[0]["old_value"], Value::Str("low".into()));
    assert_eq!(returned.rows[0]["new_value"], Value::Str("moved".into()));
    assert!(engine
        .sql("SELECT * FROM movement_targets_low", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        engine
            .sql(
                "SELECT item_key FROM movement_targets_high ORDER BY item_key",
                &[],
            )
            .unwrap()
            .rows
            .iter()
            .map(|row| row["item_key"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2)]
    );
}

#[test]
fn direct_leaf_update_rejects_sibling_movement_atomically() {
    let engine = Engine::new();
    create_range_movement_fixture(&engine);
    exec(
        &engine,
        "INSERT INTO movement_targets VALUES (1, 1, 'first'), (2, 2, 'second')",
    );
    let error = engine
        .sql(
            "UPDATE movement_targets_low SET bucket = CASE WHEN item_key = 1 THEN 3 ELSE 12 END, value = 'changed' RETURNING old.bucket, new.bucket",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23514"));
    let rows = engine
        .sql(
            "SELECT item_key, bucket, value FROM movement_targets_low ORDER BY item_key",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 2);
    assert_eq!(rows.rows[0]["bucket"], Value::Int(1));
    assert_eq!(rows.rows[0]["value"], Value::Str("first".into()));
    assert_eq!(rows.rows[1]["bucket"], Value::Int(2));
    assert_eq!(rows.rows[1]["value"], Value::Str("second".into()));
    assert!(engine
        .sql("SELECT * FROM movement_targets_high", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn update_from_spill_distinguishes_equal_leaf_doc_ids_and_is_atomic() {
    let engine = Engine::new();
    create_range_movement_fixture(&engine);
    exec(
        &engine,
        "INSERT INTO movement_targets VALUES (1, 1, 'low'), (2, 11, 'high')",
    );
    exec(
        &engine,
        "CREATE TABLE movement_source (seq INTEGER, item_key INTEGER, old_bucket INTEGER, new_bucket INTEGER, new_value TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO movement_source VALUES (1, 1, 1, 2, 'low-updated'), (2, 2, 11, 3, 'high-moved')",
    );
    exec(&engine, "SET work_mem TO '1B'");

    let returned = engine
        .sql(
            "UPDATE movement_targets AS target SET bucket = source.new_bucket, value = source.new_value FROM movement_source AS source WHERE target.item_key = source.item_key AND target.bucket = source.old_bucket RETURNING source.seq AS seq, old._doc_id AS old_doc_id, new._doc_id AS new_doc_id, old.bucket AS old_bucket, new.bucket AS new_bucket, new.value AS new_value",
            &[],
        )
        .unwrap();
    assert_eq!(returned.rows.len(), 2);
    let low = returned
        .rows
        .iter()
        .find(|row| row["seq"] == Value::Int(1))
        .unwrap();
    let high = returned
        .rows
        .iter()
        .find(|row| row["seq"] == Value::Int(2))
        .unwrap();
    assert_eq!(low["old_bucket"], Value::Int(1));
    assert_eq!(low["new_bucket"], Value::Int(2));
    assert_eq!(low["new_value"], Value::Str("low-updated".into()));
    assert_eq!(high["old_bucket"], Value::Int(11));
    assert_eq!(high["new_bucket"], Value::Int(3));
    assert_eq!(high["new_value"], Value::Str("high-moved".into()));
    assert_eq!(low["old_doc_id"], high["old_doc_id"]);
    assert_ne!(high["old_doc_id"], high["new_doc_id"]);
    assert!(engine
        .sql("SELECT * FROM movement_targets_high", &[])
        .unwrap()
        .rows
        .is_empty());

    exec(&engine, "TRUNCATE movement_source");
    exec(
        &engine,
        "INSERT INTO movement_source VALUES (3, 1, 2, 4, 'must-rollback'), (4, 2, 3, 30, 'invalid')",
    );
    let error = engine
        .sql(
            "UPDATE movement_targets AS target SET bucket = source.new_bucket, value = source.new_value FROM movement_source AS source WHERE target.item_key = source.item_key AND target.bucket = source.old_bucket RETURNING new.item_key",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23514"));
    let rows = engine
        .sql(
            "SELECT item_key, bucket, value FROM movement_targets ORDER BY item_key",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 2);
    assert_eq!(rows.rows[0]["bucket"], Value::Int(2));
    assert_eq!(rows.rows[0]["value"], Value::Str("low-updated".into()));
    assert_eq!(rows.rows[1]["bucket"], Value::Int(3));
    assert_eq!(rows.rows[1]["value"], Value::Str("high-moved".into()));
}

#[test]
fn nested_partition_updates_route_within_the_target_subtree_only() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE nested_targets (id INTEGER, region INTEGER, bucket INTEGER, value TEXT) PARTITION BY RANGE (region)",
    );
    exec(
        &engine,
        "CREATE TABLE nested_targets_local PARTITION OF nested_targets FOR VALUES FROM (0) TO (100) PARTITION BY RANGE (bucket)",
    );
    exec(
        &engine,
        "CREATE TABLE nested_targets_local_low PARTITION OF nested_targets_local FOR VALUES FROM (0) TO (10)",
    );
    exec(
        &engine,
        "CREATE TABLE nested_targets_local_high PARTITION OF nested_targets_local FOR VALUES FROM (10) TO (20)",
    );
    exec(
        &engine,
        "CREATE TABLE nested_targets_remote PARTITION OF nested_targets FOR VALUES FROM (100) TO (200)",
    );
    exec(
        &engine,
        "INSERT INTO nested_targets VALUES (1, 1, 1, 'before')",
    );

    let direct_error = engine
        .sql(
            "UPDATE nested_targets_local_low SET bucket = 11 WHERE id = 1",
            &[],
        )
        .unwrap_err();
    assert_eq!(direct_error.sqlstate(), Some("23514"));
    let within_subtree = engine
        .sql(
            "UPDATE nested_targets_local SET bucket = 11, value = 'middle' WHERE id = 1 RETURNING old.bucket AS old_bucket, new.bucket AS new_bucket, new.value AS new_value",
            &[],
        )
        .unwrap();
    assert_eq!(within_subtree.rows.len(), 1);
    assert_eq!(within_subtree.rows[0]["old_bucket"], Value::Int(1));
    assert_eq!(within_subtree.rows[0]["new_bucket"], Value::Int(11));
    assert_eq!(
        within_subtree.rows[0]["new_value"],
        Value::Str("middle".into())
    );
    assert!(engine
        .sql("SELECT * FROM nested_targets_local_low", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        engine
            .sql("SELECT bucket FROM nested_targets_local_high", &[])
            .unwrap()
            .rows[0]["bucket"],
        Value::Int(11)
    );

    let outside_subtree = engine
        .sql(
            "UPDATE nested_targets_local SET region = 101 WHERE id = 1",
            &[],
        )
        .unwrap_err();
    assert_eq!(outside_subtree.sqlstate(), Some("23514"));
    let through_root = engine
        .sql(
            "UPDATE nested_targets SET region = 101, value = 'remote' WHERE id = 1 RETURNING old.region AS old_region, new.region AS new_region, old.value AS old_value, new.value AS new_value",
            &[],
        )
        .unwrap();
    assert_eq!(through_root.rows.len(), 1);
    assert_eq!(through_root.rows[0]["old_region"], Value::Int(1));
    assert_eq!(through_root.rows[0]["new_region"], Value::Int(101));
    assert_eq!(
        through_root.rows[0]["old_value"],
        Value::Str("middle".into())
    );
    assert_eq!(
        through_root.rows[0]["new_value"],
        Value::Str("remote".into())
    );
    assert!(engine
        .sql("SELECT * FROM nested_targets_local", &[])
        .unwrap()
        .rows
        .is_empty());
    let remote = engine
        .sql(
            "SELECT id, region, bucket, value FROM nested_targets_remote",
            &[],
        )
        .unwrap();
    assert_eq!(remote.rows.len(), 1);
    assert_eq!(remote.rows[0]["region"], Value::Int(101));
    assert_eq!(remote.rows[0]["bucket"], Value::Int(11));
    assert_eq!(remote.rows[0]["value"], Value::Str("remote".into()));
}

#[test]
fn waiting_parent_update_follows_a_row_moved_to_another_partition() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("partition-movement-successor.db")).unwrap();
    create_range_movement_fixture(&root);
    exec(
        &root,
        "INSERT INTO movement_targets VALUES (1, 1, 'before')",
    );
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    exec(&holder, "BEGIN");
    exec(
        &holder,
        "SELECT item_key FROM movement_targets WHERE item_key = 1 FOR UPDATE",
    );
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "UPDATE movement_targets SET value = 'waiter' WHERE item_key = 1 RETURNING bucket, value",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    exec(
        &holder,
        "UPDATE movement_targets SET bucket = 12 WHERE item_key = 1",
    );
    exec(&holder, "COMMIT");

    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.affected_rows, 1);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["bucket"], Value::Int(12));
    assert_eq!(result.rows[0]["value"], Value::Str("waiter".into()));
    assert!(root
        .sql("SELECT * FROM movement_targets_low", &[])
        .unwrap()
        .rows
        .is_empty());
    let high = root
        .sql("SELECT bucket, value FROM movement_targets_high", &[])
        .unwrap();
    assert_eq!(high.rows.len(), 1);
    assert_eq!(high.rows[0]["bucket"], Value::Int(12));
    assert_eq!(high.rows[0]["value"], Value::Str("waiter".into()));
}

#[test]
fn waiting_update_from_spill_follows_a_row_moved_to_another_partition() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("partition-update-from-successor.db")).unwrap();
    create_range_movement_fixture(&root);
    exec(
        &root,
        "INSERT INTO movement_targets VALUES (1, 1, 'before')",
    );
    exec(
        &root,
        "CREATE TABLE movement_source (item_key INTEGER, new_value TEXT); INSERT INTO movement_source VALUES (1, 'from-waiter')",
    );
    exec(&root, "SET work_mem TO '1B'");
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    exec(&holder, "BEGIN");
    exec(
        &holder,
        "SELECT item_key FROM movement_targets WHERE item_key = 1 FOR UPDATE",
    );
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "UPDATE movement_targets AS target SET value = source.new_value FROM movement_source AS source WHERE target.item_key = source.item_key RETURNING source.new_value AS source_value, old.bucket AS old_bucket, new.bucket AS new_bucket, new.value AS new_value",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    exec(
        &holder,
        "UPDATE movement_targets SET bucket = 12 WHERE item_key = 1",
    );
    exec(&holder, "COMMIT");

    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.affected_rows, 1);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(
        result.rows[0]["source_value"],
        Value::Str("from-waiter".into())
    );
    assert_eq!(result.rows[0]["old_bucket"], Value::Int(12));
    assert_eq!(result.rows[0]["new_bucket"], Value::Int(12));
    assert_eq!(
        result.rows[0]["new_value"],
        Value::Str("from-waiter".into())
    );
    let high = root
        .sql("SELECT bucket, value FROM movement_targets_high", &[])
        .unwrap();
    assert_eq!(high.rows.len(), 1);
    assert_eq!(high.rows[0]["bucket"], Value::Int(12));
    assert_eq!(high.rows[0]["value"], Value::Str("from-waiter".into()));
}

#[test]
fn waiting_parent_delete_follows_a_row_moved_to_another_partition() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("partition-delete-successor.db")).unwrap();
    create_range_movement_fixture(&root);
    exec(
        &root,
        "INSERT INTO movement_targets VALUES (1, 1, 'before')",
    );
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    exec(&holder, "BEGIN");
    exec(
        &holder,
        "SELECT item_key FROM movement_targets WHERE item_key = 1 FOR UPDATE",
    );
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "DELETE FROM movement_targets WHERE item_key = 1 RETURNING bucket, value",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    exec(
        &holder,
        "UPDATE movement_targets SET bucket = 12 WHERE item_key = 1",
    );
    exec(&holder, "COMMIT");

    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.affected_rows, 1);
    assert_eq!(result.rows[0]["bucket"], Value::Int(12));
    assert!(root
        .sql("SELECT * FROM movement_targets", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn waiting_merge_follows_a_row_moved_to_another_partition() {
    let directory = tempfile::tempdir().unwrap();
    let root = Engine::open(&directory.path().join("partition-merge-successor.db")).unwrap();
    create_range_movement_fixture(&root);
    exec(
        &root,
        "INSERT INTO movement_targets VALUES (1, 1, 'before')",
    );
    let holder = root.new_session().unwrap();
    let waiter = root.new_session().unwrap();
    exec(&holder, "BEGIN");
    exec(
        &holder,
        "SELECT item_key FROM movement_targets WHERE item_key = 1 FOR UPDATE",
    );
    let (done_tx, done_rx) = mpsc::channel();
    let waiting_thread = std::thread::spawn(move || {
        done_tx
            .send(waiter.sql(
                "MERGE INTO movement_targets AS target USING (SELECT 1 AS item_key, 'merged' AS value) AS source ON target.item_key = source.item_key WHEN MATCHED THEN UPDATE SET value = source.value WHEN NOT MATCHED THEN INSERT (item_key, bucket, value) VALUES (source.item_key, 1, source.value) RETURNING merge_action() AS action, target.bucket AS bucket, target.value AS value",
                &[],
            ))
            .unwrap();
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(150)).is_err());
    exec(
        &holder,
        "UPDATE movement_targets SET bucket = 12 WHERE item_key = 1",
    );
    exec(&holder, "COMMIT");

    let result = done_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    waiting_thread.join().unwrap();
    assert_eq!(result.affected_rows, 1);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["action"], Value::Str("UPDATE".into()));
    assert_eq!(result.rows[0]["bucket"], Value::Int(12));
    assert_eq!(result.rows[0]["value"], Value::Str("merged".into()));
    assert_eq!(
        root.sql("SELECT count(*) AS count FROM movement_targets", &[])
            .unwrap()
            .rows[0]["count"],
        Value::Int(1)
    );
}
