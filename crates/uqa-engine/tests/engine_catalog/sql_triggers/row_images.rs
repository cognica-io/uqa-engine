//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Trigger and partition interactions with `PostgreSQL` 18 DML row images.

use super::{exec, Engine, Value};
use uqa_sql::ResultRow;

fn assert_value(row: &ResultRow, column: &str, expected: Value) {
    assert_eq!(row.get(column).cloned(), Some(expected), "column {column}");
}

fn regclass(engine: &Engine, relation: &str) -> Value {
    exec(
        engine,
        &format!("SELECT '{relation}'::regclass AS relation_oid"),
    )
    .rows[0]["relation_oid"]
        .clone()
}

#[test]
fn table_triggers_feed_returning_images_and_preserve_original_old_rows() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE returning_table_images (id INTEGER PRIMARY KEY, value INTEGER, generated_value INTEGER GENERATED ALWAYS AS (value * 10) STORED)",
    );
    exec(
        &engine,
        "CREATE FUNCTION returning_table_image_mutate() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF TG_OP = 'INSERT' THEN
             IF NEW.id = 99 THEN RETURN NULL; END IF;
             NEW.value := NEW.value + 1;
             RETURN NEW;
           ELSIF TG_OP = 'UPDATE' THEN
             OLD.value := OLD.value + 1000;
             IF OLD.id = 3 THEN RETURN NULL; END IF;
             NEW.value := NEW.value + 10;
             RETURN NEW;
           ELSE
             OLD.value := OLD.value + 1000;
             IF OLD.id = 4 THEN RETURN NULL; END IF;
             RETURN OLD;
           END IF;
         END
         $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER returning_table_image_before BEFORE INSERT OR UPDATE OR DELETE ON returning_table_images FOR EACH ROW EXECUTE FUNCTION returning_table_image_mutate()",
    );

    let inserted = exec(
        &engine,
        "INSERT INTO returning_table_images VALUES (1, 5), (3, 30), (4, 40), (99, 99) RETURNING old.id AS old_id, new.id AS new_id, new.value AS new_value, new.generated_value AS new_generated",
    );
    assert_eq!(inserted.affected_rows, 3);
    assert_eq!(inserted.rows.len(), 3);
    assert_value(&inserted.rows[0], "old_id", Value::Null);
    assert_value(&inserted.rows[0], "new_id", Value::Int(1));
    assert_value(&inserted.rows[0], "new_value", Value::Int(6));
    assert_value(&inserted.rows[0], "new_generated", Value::Int(60));

    let updated = exec(
        &engine,
        "UPDATE returning_table_images SET value = 100 WHERE id = 1 RETURNING old.value AS old_value, old.generated_value AS old_generated, new.value AS new_value, new.generated_value AS new_generated, value AS current_value",
    );
    assert_value(&updated.rows[0], "old_value", Value::Int(6));
    assert_value(&updated.rows[0], "old_generated", Value::Int(60));
    assert_value(&updated.rows[0], "new_value", Value::Int(110));
    assert_value(&updated.rows[0], "new_generated", Value::Int(1100));
    assert_value(&updated.rows[0], "current_value", Value::Int(110));

    let deleted = exec(
        &engine,
        "DELETE FROM returning_table_images WHERE id = 1 RETURNING old.value AS old_value, old.generated_value AS old_generated, new.value AS new_value, value AS current_value",
    );
    assert_value(&deleted.rows[0], "old_value", Value::Int(110));
    assert_value(&deleted.rows[0], "old_generated", Value::Int(1100));
    assert_value(&deleted.rows[0], "new_value", Value::Null);
    assert_value(&deleted.rows[0], "current_value", Value::Int(110));

    let suppressed_update = exec(
        &engine,
        "UPDATE returning_table_images SET value = 300 WHERE id = 3 RETURNING old.value, new.value",
    );
    assert_eq!(suppressed_update.affected_rows, 0);
    assert!(suppressed_update.rows.is_empty());
    let suppressed_delete = exec(
        &engine,
        "DELETE FROM returning_table_images WHERE id = 4 RETURNING old.value, new.value",
    );
    assert_eq!(suppressed_delete.affected_rows, 0);
    assert!(suppressed_delete.rows.is_empty());
    let remaining = exec(
        &engine,
        "SELECT id, value FROM returning_table_images ORDER BY id",
    );
    assert_eq!(remaining.rows.len(), 2);
    assert_value(&remaining.rows[0], "id", Value::Int(3));
    assert_value(&remaining.rows[0], "value", Value::Int(31));
    assert_value(&remaining.rows[1], "id", Value::Int(4));
    assert_value(&remaining.rows[1], "value", Value::Int(41));
}

#[test]
fn after_trigger_writes_do_not_retroactively_change_returning_images() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE returning_after_images (id INTEGER PRIMARY KEY, value INTEGER, after_seen BOOLEAN DEFAULT false)",
    );
    exec(
        &engine,
        "CREATE FUNCTION returning_after_image_mutate() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NOT NEW.after_seen THEN
             UPDATE returning_after_images SET after_seen = true WHERE id = NEW.id;
           END IF;
           RETURN NULL;
         END
         $$",
    );
    exec(
        &engine,
        "CREATE TRIGGER returning_after_insert AFTER INSERT ON returning_after_images FOR EACH ROW EXECUTE FUNCTION returning_after_image_mutate()",
    );
    exec(
        &engine,
        "CREATE TRIGGER returning_after_update AFTER UPDATE ON returning_after_images FOR EACH ROW WHEN (NOT NEW.after_seen) EXECUTE FUNCTION returning_after_image_mutate()",
    );
    let after_insert = exec(
        &engine,
        "INSERT INTO returning_after_images(id, value) VALUES (1, 10) RETURNING old.after_seen AS old_seen, new.after_seen AS new_seen, after_seen AS current_seen",
    );
    assert_value(&after_insert.rows[0], "old_seen", Value::Null);
    assert_value(&after_insert.rows[0], "new_seen", Value::Bool(false));
    assert_value(&after_insert.rows[0], "current_seen", Value::Bool(false));
    assert_value(
        &exec(
            &engine,
            "SELECT after_seen FROM returning_after_images WHERE id = 1",
        )
        .rows[0],
        "after_seen",
        Value::Bool(true),
    );
    let after_update = exec(
        &engine,
        "UPDATE returning_after_images SET value = 20, after_seen = false WHERE id = 1 RETURNING old.value AS old_value, old.after_seen AS old_seen, new.value AS new_value, new.after_seen AS new_seen, after_seen AS current_seen",
    );
    assert_value(&after_update.rows[0], "old_value", Value::Int(10));
    assert_value(&after_update.rows[0], "old_seen", Value::Bool(true));
    assert_value(&after_update.rows[0], "new_value", Value::Int(20));
    assert_value(&after_update.rows[0], "new_seen", Value::Bool(false));
    assert_value(&after_update.rows[0], "current_seen", Value::Bool(false));
    assert_value(
        &exec(
            &engine,
            "SELECT after_seen FROM returning_after_images WHERE id = 1",
        )
        .rows[0],
        "after_seen",
        Value::Bool(true),
    );
}

fn install_partition_fixture(engine: &Engine, table: &str) {
    exec(
        engine,
        &format!(
            "CREATE TABLE {table} (id INTEGER, bucket INTEGER, value INTEGER, generated_value INTEGER GENERATED ALWAYS AS (value * 10) STORED, PRIMARY KEY (id, bucket)) PARTITION BY RANGE (bucket)"
        ),
    );
    exec(
        engine,
        &format!("CREATE TABLE {table}_low PARTITION OF {table} FOR VALUES FROM (0) TO (10)"),
    );
    exec(
        engine,
        &format!("CREATE TABLE {table}_high PARTITION OF {table} FOR VALUES FROM (10) TO (20)"),
    );
    exec(
        engine,
        &format!(
            "CREATE FUNCTION {table}_mutate() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
               IF TG_OP = 'INSERT' THEN NEW.value := NEW.value + 1; RETURN NEW;
               ELSIF TG_OP = 'UPDATE' THEN NEW.value := NEW.value + 10; RETURN NEW;
               ELSE RETURN OLD;
               END IF;
             END
             $$"
        ),
    );
    exec(
        engine,
        &format!(
            "CREATE TRIGGER {table}_before BEFORE INSERT OR UPDATE OR DELETE ON {table} FOR EACH ROW EXECUTE FUNCTION {table}_mutate()"
        ),
    );
}

#[test]
fn partitioned_returning_images_preserve_leaf_identity_generated_values_and_upsert_triggers() {
    let engine = Engine::new();
    install_partition_fixture(&engine, "returning_partition_images");
    let low = regclass(&engine, "returning_partition_images_low");
    let high = regclass(&engine, "returning_partition_images_high");
    let inserted = exec(
        &engine,
        "INSERT INTO returning_partition_images VALUES (1, 1, 5), (2, 11, 6), (3, 1, 30) RETURNING old.tableoid::regclass AS old_leaf, new.tableoid::regclass AS new_leaf, new.id AS new_id, new.value AS new_value, new.generated_value AS new_generated, tableoid::regclass AS current_leaf",
    );
    assert_eq!(inserted.rows.len(), 3);
    assert_value(&inserted.rows[0], "old_leaf", Value::Null);
    assert_value(&inserted.rows[0], "new_leaf", low.clone());
    assert_value(&inserted.rows[0], "new_value", Value::Int(6));
    assert_value(&inserted.rows[0], "new_generated", Value::Int(60));
    assert_value(&inserted.rows[1], "new_leaf", high.clone());
    assert_value(&inserted.rows[1], "new_value", Value::Int(7));

    let same_leaf = exec(
        &engine,
        "UPDATE returning_partition_images SET value = value + 100 WHERE id = 1 RETURNING old.tableoid::regclass AS old_leaf, old.value AS old_value, old.generated_value AS old_generated, new.tableoid::regclass AS new_leaf, new.value AS new_value, new.generated_value AS new_generated, tableoid::regclass AS current_leaf",
    );
    assert_value(&same_leaf.rows[0], "old_leaf", low.clone());
    assert_value(&same_leaf.rows[0], "old_value", Value::Int(6));
    assert_value(&same_leaf.rows[0], "old_generated", Value::Int(60));
    assert_value(&same_leaf.rows[0], "new_value", Value::Int(116));
    assert_value(&same_leaf.rows[0], "new_generated", Value::Int(1160));

    let moved = exec(
        &engine,
        "UPDATE returning_partition_images SET bucket = 12, value = value + 100 WHERE id = 1 RETURNING old.tableoid::regclass AS old_leaf, old.bucket AS old_bucket, old.value AS old_value, new.tableoid::regclass AS new_leaf, new.bucket AS new_bucket, new.value AS new_value, new.generated_value AS new_generated, tableoid::regclass AS current_leaf",
    );
    assert_value(&moved.rows[0], "old_leaf", low.clone());
    assert_value(&moved.rows[0], "old_bucket", Value::Int(1));
    assert_value(&moved.rows[0], "old_value", Value::Int(116));
    assert_value(&moved.rows[0], "new_leaf", high.clone());
    assert_value(&moved.rows[0], "new_bucket", Value::Int(12));
    assert_value(&moved.rows[0], "new_value", Value::Int(227));
    assert_value(&moved.rows[0], "new_generated", Value::Int(2270));

    let upserted = exec(
        &engine,
        "INSERT INTO returning_partition_images VALUES (1, 12, 20) ON CONFLICT (id, bucket) DO UPDATE SET value = excluded.value + 100 RETURNING old.tableoid::regclass AS old_leaf, old.value AS old_value, new.tableoid::regclass AS new_leaf, new.value AS new_value, tableoid::regclass AS current_leaf",
    );
    assert_value(&upserted.rows[0], "old_value", Value::Int(227));
    assert_value(&upserted.rows[0], "new_value", Value::Int(131));
    assert_value(&upserted.rows[0], "current_leaf", high.clone());

    let deleted = exec(
        &engine,
        "DELETE FROM returning_partition_images WHERE id = 2 RETURNING old.tableoid::regclass AS old_leaf, old.value AS old_value, old.generated_value AS old_generated, new.tableoid::regclass AS new_leaf, new.value AS new_value, tableoid::regclass AS current_leaf",
    );
    assert_value(&deleted.rows[0], "old_leaf", high.clone());
    assert_value(&deleted.rows[0], "old_value", Value::Int(7));
    assert_value(&deleted.rows[0], "old_generated", Value::Int(70));
    assert_value(&deleted.rows[0], "new_leaf", Value::Null);
    assert_value(&deleted.rows[0], "new_value", Value::Null);
    assert_value(&deleted.rows[0], "current_leaf", high);
}

#[test]
fn partitioned_merge_returning_uses_each_action_post_trigger_image_and_physical_leaf() {
    let engine = Engine::new();
    install_partition_fixture(&engine, "returning_merge_images");
    let low = regclass(&engine, "returning_merge_images_low");
    let high = regclass(&engine, "returning_merge_images_high");
    exec(
        &engine,
        "INSERT INTO returning_merge_images VALUES (1, 1, 10), (2, 11, 20), (3, 1, 30)",
    );
    exec(
        &engine,
        "CREATE TABLE returning_merge_source (id INTEGER, bucket INTEGER, value INTEGER, action TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO returning_merge_source VALUES (1, 1, 100, 'update'), (2, 11, 200, 'delete'), (3, 12, 300, 'move'), (4, 11, 400, 'insert')",
    );
    let result = exec(
        &engine,
        "MERGE INTO returning_merge_images AS target
         USING returning_merge_source AS source ON target.id = source.id
         WHEN MATCHED AND source.action IN ('update', 'move') THEN UPDATE SET bucket = source.bucket, value = source.value
         WHEN MATCHED AND source.action = 'delete' THEN DELETE
         WHEN NOT MATCHED THEN INSERT (id, bucket, value) VALUES (source.id, source.bucket, source.value)
         RETURNING merge_action() AS command, source.action AS source_action,
                   old.tableoid::regclass AS old_leaf, old.value AS old_value,
                   new.tableoid::regclass AS new_leaf, new.value AS new_value,
                   target.tableoid::regclass AS current_leaf, target.value AS current_value",
    );
    assert_eq!(result.affected_rows, 4);
    let action = |name: &str| {
        result
            .rows
            .iter()
            .find(|row| row.get("source_action") == Some(&Value::Str(name.into())))
            .unwrap_or_else(|| panic!("missing MERGE action {name}: {result:?}"))
    };
    let updated = action("update");
    assert_value(updated, "command", Value::Str("UPDATE".into()));
    assert_value(updated, "old_value", Value::Int(11));
    assert_value(updated, "new_value", Value::Int(110));
    assert_value(updated, "new_leaf", low.clone());
    let deleted = action("delete");
    assert_value(deleted, "command", Value::Str("DELETE".into()));
    assert_value(deleted, "old_value", Value::Int(21));
    assert_value(deleted, "new_leaf", Value::Null);
    assert_value(deleted, "current_leaf", high.clone());
    let moved = action("move");
    assert_value(moved, "old_value", Value::Int(31));
    assert_value(moved, "new_value", Value::Int(311));
    assert_value(moved, "old_leaf", low);
    assert_value(moved, "new_leaf", high.clone());
    let inserted = action("insert");
    assert_value(inserted, "command", Value::Str("INSERT".into()));
    assert_value(inserted, "old_leaf", Value::Null);
    assert_value(inserted, "new_value", Value::Int(401));
    assert_value(inserted, "new_leaf", high);
}
