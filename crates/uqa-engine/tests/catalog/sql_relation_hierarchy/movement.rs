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
fn partition_moving_update_uses_delete_and_insert_row_trigger_lifecycle() {
    let engine = Engine::new();
    create_range_movement_fixture(&engine);
    exec(
        &engine,
        "CREATE TABLE movement_trigger_log (seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, message TEXT NOT NULL)",
    );
    exec(
        &engine,
        "CREATE FUNCTION movement_trigger_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN IF TG_LEVEL = 'STATEMENT' THEN INSERT INTO movement_trigger_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME || ':' || TG_LEVEL); RETURN NULL; ELSIF TG_OP = 'DELETE' THEN INSERT INTO movement_trigger_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME || ':' || TG_LEVEL || ':' || OLD.item_key::text || ':' || OLD.bucket::text || ':NULL:NULL'); RETURN OLD; ELSIF TG_OP = 'INSERT' THEN INSERT INTO movement_trigger_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME || ':' || TG_LEVEL || ':NULL:NULL:' || NEW.item_key::text || ':' || NEW.bucket::text); RETURN NEW; ELSE INSERT INTO movement_trigger_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME || ':' || TG_LEVEL || ':' || OLD.item_key::text || ':' || OLD.bucket::text || ':' || NEW.item_key::text || ':' || NEW.bucket::text); RETURN NEW; END IF; END $probe$",
    );
    exec(
        &engine,
        "CREATE FUNCTION movement_transition_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ DECLARE old_count BIGINT; new_count BIGINT; old_bucket_sum BIGINT; new_bucket_sum BIGINT; BEGIN SELECT count(*), coalesce(sum(bucket), 0) INTO old_count, old_bucket_sum FROM old_rows; SELECT count(*), coalesce(sum(bucket), 0) INTO new_count, new_bucket_sum FROM new_rows; INSERT INTO movement_trigger_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME || ':' || TG_LEVEL || ':old=' || old_count::text || '/' || old_bucket_sum::text || ':new=' || new_count::text || '/' || new_bucket_sum::text); RETURN NULL; END $probe$",
    );
    for trigger in [
        "CREATE TRIGGER parent_before_update BEFORE UPDATE ON movement_targets FOR EACH ROW EXECUTE FUNCTION movement_trigger_probe()",
        "CREATE TRIGGER parent_after_update AFTER UPDATE ON movement_targets FOR EACH ROW EXECUTE FUNCTION movement_trigger_probe()",
        "CREATE TRIGGER parent_before_update_statement BEFORE UPDATE ON movement_targets FOR EACH STATEMENT EXECUTE FUNCTION movement_trigger_probe()",
        "CREATE TRIGGER parent_after_update_statement AFTER UPDATE ON movement_targets REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION movement_transition_probe()",
        "CREATE TRIGGER low_before_update BEFORE UPDATE ON movement_targets_low FOR EACH ROW EXECUTE FUNCTION movement_trigger_probe()",
        "CREATE TRIGGER low_after_update AFTER UPDATE ON movement_targets_low FOR EACH ROW EXECUTE FUNCTION movement_trigger_probe()",
        "CREATE TRIGGER low_before_delete BEFORE DELETE ON movement_targets_low FOR EACH ROW EXECUTE FUNCTION movement_trigger_probe()",
        "CREATE TRIGGER low_after_delete AFTER DELETE ON movement_targets_low FOR EACH ROW EXECUTE FUNCTION movement_trigger_probe()",
        "CREATE TRIGGER high_before_insert BEFORE INSERT ON movement_targets_high FOR EACH ROW EXECUTE FUNCTION movement_trigger_probe()",
        "CREATE TRIGGER high_after_insert AFTER INSERT ON movement_targets_high FOR EACH ROW EXECUTE FUNCTION movement_trigger_probe()",
    ] {
        exec(&engine, trigger);
    }
    exec(
        &engine,
        "INSERT INTO movement_targets VALUES (1, 1, 'before')",
    );

    let returned = engine
        .sql(
            "UPDATE movement_targets SET bucket = 11, value = 'after' WHERE item_key = 1 RETURNING old.item_key AS old_item_key, old.bucket AS old_bucket, new.item_key AS new_item_key, new.bucket AS new_bucket, new.value AS new_value",
            &[],
        )
        .unwrap();
    assert_eq!(returned.rows.len(), 1);
    assert_eq!(returned.rows[0]["old_item_key"], Value::Int(1));
    assert_eq!(returned.rows[0]["old_bucket"], Value::Int(1));
    assert_eq!(returned.rows[0]["new_item_key"], Value::Int(1));
    assert_eq!(returned.rows[0]["new_bucket"], Value::Int(11));
    assert_eq!(returned.rows[0]["new_value"], Value::Str("after".into()));

    let messages = engine
        .sql("SELECT message FROM movement_trigger_log ORDER BY seq", &[])
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row["message"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        messages,
        vec![
            Value::Str(
                "parent_before_update_statement:BEFORE:UPDATE:movement_targets:STATEMENT".into()
            ),
            Value::Str("low_before_update:BEFORE:UPDATE:movement_targets_low:ROW:1:1:1:11".into()),
            Value::Str(
                "parent_before_update:BEFORE:UPDATE:movement_targets_low:ROW:1:1:1:11".into()
            ),
            Value::Str(
                "low_before_delete:BEFORE:DELETE:movement_targets_low:ROW:1:1:NULL:NULL".into()
            ),
            Value::Str(
                "high_before_insert:BEFORE:INSERT:movement_targets_high:ROW:NULL:NULL:1:11".into()
            ),
            Value::Str(
                "low_after_delete:AFTER:DELETE:movement_targets_low:ROW:1:1:NULL:NULL".into()
            ),
            Value::Str(
                "high_after_insert:AFTER:INSERT:movement_targets_high:ROW:NULL:NULL:1:11".into()
            ),
            Value::Str(
                "parent_after_update_statement:AFTER:UPDATE:movement_targets:STATEMENT:old=1/1:new=1/11".into()
            ),
        ]
    );
}

fn install_partition_move_cancellation_fixture(engine: &Engine) {
    create_range_movement_fixture(engine);
    exec(
        engine,
        "CREATE TABLE movement_cancel_log (seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, message TEXT NOT NULL)",
    );
    exec(
        engine,
        "CREATE FUNCTION movement_cancel_log_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO movement_cancel_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME); IF TG_OP = 'DELETE' THEN RETURN OLD; END IF; RETURN NEW; END $probe$",
    );
    exec(
        engine,
        "CREATE FUNCTION movement_cancel_row() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO movement_cancel_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME); RETURN NULL; END $probe$",
    );
    exec(
        engine,
        "CREATE FUNCTION movement_cancel_transition_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ DECLARE old_count BIGINT; new_count BIGINT; old_bucket_sum BIGINT; new_bucket_sum BIGINT; BEGIN SELECT count(*), coalesce(sum(bucket), 0) INTO old_count, old_bucket_sum FROM old_rows; SELECT count(*), coalesce(sum(bucket), 0) INTO new_count, new_bucket_sum FROM new_rows; INSERT INTO movement_cancel_log(message) VALUES ('transition:' || old_count::text || '/' || old_bucket_sum::text || ':' || new_count::text || '/' || new_bucket_sum::text); RETURN NULL; END $probe$",
    );
    for trigger in [
        "CREATE TRIGGER source_cancel_delete BEFORE DELETE ON movement_targets_low FOR EACH ROW EXECUTE FUNCTION movement_cancel_row()",
        "CREATE TRIGGER source_after_delete AFTER DELETE ON movement_targets_low FOR EACH ROW EXECUTE FUNCTION movement_cancel_log_probe()",
        "CREATE TRIGGER destination_before_insert BEFORE INSERT ON movement_targets_high FOR EACH ROW EXECUTE FUNCTION movement_cancel_log_probe()",
        "CREATE TRIGGER destination_after_insert AFTER INSERT ON movement_targets_high FOR EACH ROW EXECUTE FUNCTION movement_cancel_log_probe()",
        "CREATE TRIGGER movement_cancel_transition AFTER UPDATE ON movement_targets REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION movement_cancel_transition_probe()",
    ] {
        exec(engine, trigger);
    }
    exec(
        engine,
        "INSERT INTO movement_targets VALUES (1, 1, 'before')",
    );
}

#[test]
fn partition_move_before_delete_and_insert_cancellation_matches_postgresql() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory
        .path()
        .join("partition-move-trigger-cancellation.db");
    let engine = Engine::open(&database).unwrap();
    install_partition_move_cancellation_fixture(&engine);

    let delete_cancelled = engine
        .sql(
            "UPDATE movement_targets SET bucket = 11 WHERE item_key = 1",
            &[],
        )
        .unwrap();
    assert_eq!(delete_cancelled.affected_rows, 0);
    assert_eq!(
        engine
            .sql("SELECT item_key, bucket FROM movement_targets_low", &[],)
            .unwrap()
            .rows[0]["bucket"],
        Value::Int(1)
    );
    let delete_cancel_messages = engine
        .sql("SELECT message FROM movement_cancel_log ORDER BY seq", &[])
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row["message"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        delete_cancel_messages,
        vec![
            Value::Str("source_cancel_delete:BEFORE:DELETE:movement_targets_low".into()),
            Value::Str("transition:0/0:0/0".into()),
        ]
    );

    drop(engine);
    let engine = Engine::open(&database).unwrap();
    exec(
        &engine,
        "DROP TRIGGER source_cancel_delete ON movement_targets_low; CREATE TRIGGER source_before_delete BEFORE DELETE ON movement_targets_low FOR EACH ROW EXECUTE FUNCTION movement_cancel_log_probe(); CREATE TRIGGER destination_cancel_insert BEFORE INSERT ON movement_targets_high FOR EACH ROW EXECUTE FUNCTION movement_cancel_row(); DELETE FROM movement_cancel_log",
    );
    let insert_cancelled = engine
        .sql(
            "UPDATE movement_targets SET bucket = 11 WHERE item_key = 1 RETURNING old.item_key, new.item_key",
            &[],
        )
        .unwrap();
    assert_eq!(insert_cancelled.affected_rows, 0);
    assert!(insert_cancelled.rows.is_empty());
    assert!(engine
        .sql("SELECT * FROM movement_targets", &[])
        .unwrap()
        .rows
        .is_empty());
    let insert_cancel_messages = engine
        .sql("SELECT message FROM movement_cancel_log ORDER BY seq", &[])
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row["message"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        insert_cancel_messages,
        vec![
            Value::Str("source_before_delete:BEFORE:DELETE:movement_targets_low".into()),
            Value::Str("destination_before_insert:BEFORE:INSERT:movement_targets_high".into()),
            Value::Str("destination_cancel_insert:BEFORE:INSERT:movement_targets_high".into()),
            Value::Str("source_after_delete:AFTER:DELETE:movement_targets_low".into()),
            Value::Str("transition:1/1:0/0".into()),
        ]
    );
}

#[test]
fn cancelled_partition_move_insert_does_not_run_delete_referential_actions() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE movement_cancel_parent (id INTEGER, bucket INTEGER, PRIMARY KEY (id, bucket)) PARTITION BY RANGE (bucket)",
    );
    exec(
        &engine,
        "CREATE TABLE movement_cancel_parent_low PARTITION OF movement_cancel_parent FOR VALUES FROM (0) TO (10)",
    );
    exec(
        &engine,
        "CREATE TABLE movement_cancel_parent_high PARTITION OF movement_cancel_parent FOR VALUES FROM (10) TO (20)",
    );
    exec(
        &engine,
        "CREATE TABLE movement_cancel_child (id INTEGER, bucket INTEGER, FOREIGN KEY (id, bucket) REFERENCES movement_cancel_parent(id, bucket) ON UPDATE CASCADE ON DELETE CASCADE)",
    );
    exec(
        &engine,
        "CREATE FUNCTION movement_cancel_destination_insert() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN RETURN NULL; END $probe$",
    );
    exec(
        &engine,
        "CREATE TRIGGER cancel_destination_insert BEFORE INSERT ON movement_cancel_parent_high FOR EACH ROW EXECUTE FUNCTION movement_cancel_destination_insert()",
    );
    exec(
        &engine,
        "INSERT INTO movement_cancel_parent VALUES (1, 1); INSERT INTO movement_cancel_child VALUES (1, 1)",
    );

    let result = engine
        .sql(
            "UPDATE movement_cancel_parent SET bucket = 11 WHERE id = 1 RETURNING old.id, new.id",
            &[],
        )
        .unwrap();
    assert_eq!(result.affected_rows, 0);
    assert!(result.rows.is_empty());
    assert!(engine
        .sql("SELECT * FROM movement_cancel_parent", &[])
        .unwrap()
        .rows
        .is_empty());
    let child = engine
        .sql("SELECT id, bucket FROM movement_cancel_child", &[])
        .unwrap();
    assert_eq!(child.rows.len(), 1);
    assert_eq!(child.rows[0]["id"], Value::Int(1));
    assert_eq!(child.rows[0]["bucket"], Value::Int(1));
}

#[test]
fn merge_partition_movement_uses_physical_triggers_and_empty_update_transitions() {
    let engine = Engine::new();
    create_range_movement_fixture(&engine);
    exec(
        &engine,
        "CREATE TABLE movement_merge_log (seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, message TEXT NOT NULL); CREATE TABLE movement_merge_source (item_key INTEGER PRIMARY KEY, bucket INTEGER)",
    );
    exec(
        &engine,
        "CREATE FUNCTION movement_merge_log_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO movement_merge_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME); IF TG_OP = 'DELETE' THEN RETURN OLD; END IF; RETURN NEW; END $probe$",
    );
    exec(
        &engine,
        "CREATE FUNCTION movement_merge_cancel_insert() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO movement_merge_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME); RETURN NULL; END $probe$",
    );
    exec(
        &engine,
        "CREATE FUNCTION movement_merge_transition_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ DECLARE old_count BIGINT; new_count BIGINT; BEGIN SELECT count(*) INTO old_count FROM old_rows; SELECT count(*) INTO new_count FROM new_rows; INSERT INTO movement_merge_log(message) VALUES ('transition:' || old_count::text || ':' || new_count::text); RETURN NULL; END $probe$",
    );
    exec(
        &engine,
        "CREATE TRIGGER source_after_delete AFTER DELETE ON movement_targets_low FOR EACH ROW EXECUTE FUNCTION movement_merge_log_probe(); CREATE TRIGGER destination_before_insert BEFORE INSERT ON movement_targets_high FOR EACH ROW EXECUTE FUNCTION movement_merge_log_probe(); CREATE TRIGGER destination_after_insert AFTER INSERT ON movement_targets_high FOR EACH ROW EXECUTE FUNCTION movement_merge_log_probe(); CREATE TRIGGER movement_merge_transition AFTER UPDATE ON movement_targets REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION movement_merge_transition_probe()",
    );
    exec(
        &engine,
        "INSERT INTO movement_targets VALUES (1, 1, 'success'); INSERT INTO movement_merge_source VALUES (1, 11)",
    );

    let moved = engine
        .sql(
            "MERGE INTO movement_targets AS target USING movement_merge_source AS source ON target.item_key = source.item_key WHEN MATCHED THEN UPDATE SET bucket = source.bucket",
            &[],
        )
        .unwrap();
    assert_eq!(moved.affected_rows, 1);
    assert_eq!(
        engine
            .sql("SELECT bucket FROM movement_targets_high", &[])
            .unwrap()
            .rows[0]["bucket"],
        Value::Int(11)
    );
    let moved_messages = engine
        .sql("SELECT message FROM movement_merge_log ORDER BY seq", &[])
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row["message"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        moved_messages,
        vec![
            Value::Str("destination_before_insert:BEFORE:INSERT:movement_targets_high".into()),
            Value::Str("source_after_delete:AFTER:DELETE:movement_targets_low".into()),
            Value::Str("destination_after_insert:AFTER:INSERT:movement_targets_high".into()),
            Value::Str("transition:0:0".into()),
        ]
    );

    exec(
        &engine,
        "CREATE TRIGGER destination_cancel_insert BEFORE INSERT ON movement_targets_high FOR EACH ROW EXECUTE FUNCTION movement_merge_cancel_insert(); INSERT INTO movement_targets VALUES (2, 2, 'cancel'); INSERT INTO movement_merge_source VALUES (2, 12); DELETE FROM movement_merge_log",
    );
    let cancelled = engine
        .sql(
            "MERGE INTO movement_targets AS target USING movement_merge_source AS source ON target.item_key = source.item_key WHEN MATCHED AND target.item_key = 2 THEN UPDATE SET bucket = source.bucket",
            &[],
        )
        .unwrap();
    assert_eq!(cancelled.affected_rows, 0);
    assert!(engine
        .sql("SELECT * FROM movement_targets WHERE item_key = 2", &[],)
        .unwrap()
        .rows
        .is_empty());
    let cancelled_messages = engine
        .sql("SELECT message FROM movement_merge_log ORDER BY seq", &[])
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row["message"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        cancelled_messages,
        vec![
            Value::Str("destination_before_insert:BEFORE:INSERT:movement_targets_high".into()),
            Value::Str("destination_cancel_insert:BEFORE:INSERT:movement_targets_high".into()),
            Value::Str("source_after_delete:AFTER:DELETE:movement_targets_low".into()),
            Value::Str("transition:0:0".into()),
        ]
    );
}

#[test]
fn partition_move_destination_trigger_can_modify_values_but_not_the_selected_partition() {
    let engine = Engine::new();
    create_range_movement_fixture(&engine);
    exec(
        &engine,
        "CREATE TABLE movement_targets_other PARTITION OF movement_targets FOR VALUES FROM (20) TO (30)",
    );
    exec(
        &engine,
        "CREATE FUNCTION movement_modify_destination() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN IF NEW.item_key = 1 THEN NEW.value := 'changed-by-trigger'; ELSIF NEW.item_key = 2 THEN NEW.bucket := 21; END IF; RETURN NEW; END $probe$",
    );
    exec(
        &engine,
        "CREATE TRIGGER modify_destination BEFORE INSERT ON movement_targets_high FOR EACH ROW EXECUTE FUNCTION movement_modify_destination()",
    );
    exec(
        &engine,
        "INSERT INTO movement_targets VALUES (1, 1, 'before'), (2, 2, 'reroute')",
    );

    let changed = engine
        .sql(
            "UPDATE movement_targets SET bucket = 11 WHERE item_key = 1 RETURNING old.value AS old_value, new.bucket AS new_bucket, new.value AS new_value",
            &[],
        )
        .unwrap();
    assert_eq!(changed.rows.len(), 1);
    assert_eq!(changed.rows[0]["old_value"], Value::Str("before".into()));
    assert_eq!(changed.rows[0]["new_bucket"], Value::Int(11));
    assert_eq!(
        changed.rows[0]["new_value"],
        Value::Str("changed-by-trigger".into())
    );

    let error = engine
        .sql(
            "UPDATE movement_targets SET bucket = 12 WHERE item_key = 2",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23514"));
    let unchanged = engine
        .sql(
            "SELECT item_key, bucket, value FROM movement_targets_low WHERE item_key = 2",
            &[],
        )
        .unwrap();
    assert_eq!(unchanged.rows.len(), 1);
    assert_eq!(unchanged.rows[0]["bucket"], Value::Int(2));
    assert_eq!(unchanged.rows[0]["value"], Value::Str("reroute".into()));
    assert!(engine
        .sql("SELECT * FROM movement_targets_other", &[])
        .unwrap()
        .rows
        .is_empty());
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
