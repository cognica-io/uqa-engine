//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 table inheritance and declarative partition routing.

use uqa_core::Value;
use uqa_engine::Engine;

#[path = "sql_relation_hierarchy/regressions.rs"]
mod regressions;

fn exec(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap();
}

fn assert_measurement_partition_catalog(engine: &Engine) {
    let classes = engine
        .sql(
            "SELECT relname, relkind, relispartition, relhassubclass FROM pg_catalog.pg_class WHERE relname IN ('measurements', 'measurements_low', 'measurements_high') ORDER BY relname",
            &[],
        )
        .unwrap();
    assert_eq!(classes.rows.len(), 3);
    assert_eq!(classes.rows[0]["relkind"], Value::Str("p".into()));
    assert_eq!(classes.rows[0]["relhassubclass"], Value::Bool(true));
    assert_eq!(classes.rows[1]["relispartition"], Value::Bool(true));
    assert_eq!(classes.rows[2]["relispartition"], Value::Bool(true));
    let inheritance = engine
        .sql(
            "SELECT child.relname AS child, parent.relname AS parent, i.inhseqno FROM pg_catalog.pg_inherits AS i JOIN pg_catalog.pg_class AS child ON child.oid = i.inhrelid JOIN pg_catalog.pg_class AS parent ON parent.oid = i.inhparent ORDER BY child.relname",
            &[],
        )
        .unwrap();
    assert_eq!(inheritance.rows.len(), 2);
    assert_eq!(
        inheritance.rows[0]["parent"],
        Value::Str("measurements".into())
    );
    assert_eq!(inheritance.rows[0]["inhseqno"], Value::Int(1));
}

fn insert_and_assert_measurement_partition_rows(engine: &Engine) {
    let returning = engine
        .sql(
            "INSERT INTO measurements (id, base) VALUES (1, 3), (11, 4) RETURNING id, doubled",
            &[],
        )
        .unwrap();
    assert_eq!(returning.rows.len(), 2);
    assert_eq!(returning.rows[0]["doubled"], Value::Int(6));
    assert_eq!(returning.rows[1]["doubled"], Value::Int(8));
    let rows = engine
        .sql(
            "SELECT id, base, doubled FROM measurements ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 2);
    assert_eq!(rows.rows[0]["id"], Value::Int(1));
    assert_eq!(rows.rows[1]["id"], Value::Int(11));
    assert!(engine
        .sql("SELECT * FROM ONLY measurements", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        engine
            .sql("SELECT id FROM measurements_low", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(1)
    );
    assert_eq!(
        engine
            .sql("SELECT id FROM measurements_high", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(11)
    );
}

fn update_and_delete_measurement_partitions(engine: &Engine) {
    let wrong_partition = engine
        .sql(
            "INSERT INTO measurements_low (id, base) VALUES (20, 1)",
            &[],
        )
        .unwrap_err();
    assert_eq!(wrong_partition.sqlstate(), Some("23514"));
    assert_range_partition_mutations(engine);
}

fn assert_range_partition_mutations(engine: &Engine) {
    let moved = engine
        .sql(
            "UPDATE measurements SET id = 12, base = 5 WHERE id = 1 RETURNING old.id AS old_id, new.id AS new_id, old.doubled AS old_doubled, new.doubled AS new_doubled",
            &[],
        )
        .unwrap();
    assert_eq!(moved.rows.len(), 1);
    assert_eq!(moved.rows[0]["old_id"], Value::Int(1));
    assert_eq!(moved.rows[0]["new_id"], Value::Int(12));
    assert_eq!(moved.rows[0]["old_doubled"], Value::Int(6));
    assert_eq!(moved.rows[0]["new_doubled"], Value::Int(10));
    assert!(engine
        .sql("SELECT * FROM measurements_low", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        engine
            .sql("SELECT id FROM measurements_high ORDER BY id", &[])
            .unwrap()
            .rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(11), Value::Int(12)]
    );
    let deleted = engine
        .sql(
            "DELETE FROM measurements WHERE id = 11 RETURNING old.id AS old_id, new.id IS NULL AS new_missing",
            &[],
        )
        .unwrap();
    assert_eq!(deleted.rows.len(), 1);
    assert_eq!(deleted.rows[0]["old_id"], Value::Int(11));
    assert_eq!(deleted.rows[0]["new_missing"], Value::Bool(true));
}

fn update_from_and_delete_using_measurement_partitions(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE measurement_adjustments (old_id INTEGER, new_id INTEGER, new_base INTEGER)",
    );
    exec(
        engine,
        "INSERT INTO measurement_adjustments VALUES (12, 2, 6)",
    );
    let moved_from = engine
        .sql(
            "UPDATE measurements AS m SET id = a.new_id, base = a.new_base FROM measurement_adjustments AS a WHERE m.id = a.old_id RETURNING old.id AS old_id, new.id AS new_id, new.doubled AS new_doubled",
            &[],
        )
        .unwrap();
    assert_eq!(moved_from.rows.len(), 1);
    assert_eq!(moved_from.rows[0]["old_id"], Value::Int(12));
    assert_eq!(moved_from.rows[0]["new_id"], Value::Int(2));
    assert_eq!(moved_from.rows[0]["new_doubled"], Value::Int(12));
    assert!(engine
        .sql("SELECT * FROM measurements_high", &[])
        .unwrap()
        .rows
        .is_empty());
    let deleted_using = engine
        .sql(
            "DELETE FROM measurements AS m USING measurement_adjustments AS a WHERE m.id = a.new_id RETURNING old.id AS old_id",
            &[],
        )
        .unwrap();
    assert_eq!(deleted_using.rows.len(), 1);
    assert_eq!(deleted_using.rows[0]["old_id"], Value::Int(2));
}

fn assert_measurement_partition_lifecycle(engine: &Engine) {
    let overlap = engine
        .sql(
            "CREATE TABLE measurements_overlap PARTITION OF measurements FOR VALUES FROM (5) TO (15)",
            &[],
        )
        .unwrap_err();
    assert_eq!(overlap.sqlstate(), Some("42P17"));
    assert!(!engine.has_table("measurements_overlap").unwrap());
    let truncate_only = engine.sql("TRUNCATE ONLY measurements", &[]).unwrap_err();
    assert_eq!(truncate_only.sqlstate(), Some("42809"));
    exec(engine, "INSERT INTO measurements VALUES (1, 1), (11, 1)");
    exec(engine, "TRUNCATE measurements");
    assert!(engine
        .sql("SELECT * FROM measurements", &[])
        .unwrap()
        .rows
        .is_empty());
    exec(engine, "DROP TABLE measurements");
    assert!(!engine.has_table("measurements").unwrap());
    assert!(!engine.has_table("measurements_low").unwrap());
    assert!(!engine.has_table("measurements_high").unwrap());
}

#[test]
fn range_partitions_route_rows_scan_descendants_and_honor_only() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE measurements (id INTEGER, base INTEGER, doubled INTEGER GENERATED ALWAYS AS (base * 2) STORED) PARTITION BY RANGE (id)",
    );
    exec(
        &engine,
        "CREATE TABLE measurements_low PARTITION OF measurements FOR VALUES FROM (MINVALUE) TO (10)",
    );
    exec(
        &engine,
        "CREATE TABLE measurements_high PARTITION OF measurements FOR VALUES FROM (10) TO (MAXVALUE)",
    );
    assert_measurement_partition_catalog(&engine);
    insert_and_assert_measurement_partition_rows(&engine);
    update_and_delete_measurement_partitions(&engine);
    update_from_and_delete_using_measurement_partitions(&engine);
    assert_measurement_partition_lifecycle(&engine);
}

#[test]
fn list_and_default_partitions_route_values_and_reject_overlap() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE events (kind TEXT, value INTEGER) PARTITION BY LIST (kind)",
    );
    exec(
        &engine,
        "CREATE TABLE events_known PARTITION OF events FOR VALUES IN ('alpha', 'beta')",
    );
    exec(
        &engine,
        "CREATE TABLE events_other PARTITION OF events DEFAULT",
    );
    exec(
        &engine,
        "INSERT INTO events VALUES ('alpha', 1), ('other', 2), (NULL, 3)",
    );
    assert_eq!(
        engine
            .sql("SELECT value FROM events_known", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
    assert_eq!(
        engine
            .sql("SELECT value FROM events_other ORDER BY value", &[])
            .unwrap()
            .rows
            .len(),
        2
    );
    let overlap = engine
        .sql(
            "CREATE TABLE events_overlap PARTITION OF events FOR VALUES IN ('beta')",
            &[],
        )
        .unwrap_err();
    assert_eq!(overlap.sqlstate(), Some("42P17"));
    let second_default = engine
        .sql(
            "CREATE TABLE events_default_again PARTITION OF events DEFAULT",
            &[],
        )
        .unwrap_err();
    assert_eq!(second_default.sqlstate(), Some("42P17"));
}

#[test]
fn inherited_generated_columns_scan_children_and_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hierarchy.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE parent_values (a INTEGER, b INTEGER GENERATED ALWAYS AS (a + 1) STORED)",
        );
        exec(
            &engine,
            "CREATE TABLE child_values (label TEXT) INHERITS (parent_values)",
        );
        exec(
            &engine,
            "INSERT INTO child_values (a, label) VALUES (7, 'child')",
        );
        let rows = engine.sql("SELECT a, b FROM parent_values", &[]).unwrap();
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.rows[0]["a"], Value::Int(7));
        assert_eq!(rows.rows[0]["b"], Value::Int(8));
        assert!(engine
            .sql("SELECT * FROM ONLY parent_values", &[])
            .unwrap()
            .rows
            .is_empty());
        let updated = engine
            .sql(
                "UPDATE parent_values SET a = 8 RETURNING old.a AS old_a, new.b AS new_b",
                &[],
            )
            .unwrap();
        assert_eq!(updated.rows.len(), 1);
        assert_eq!(updated.rows[0]["old_a"], Value::Int(7));
        assert_eq!(updated.rows[0]["new_b"], Value::Int(9));
        exec(&engine, "INSERT INTO parent_values (a) VALUES (1)");
        exec(&engine, "TRUNCATE ONLY parent_values");
        assert_eq!(
            engine
                .sql("SELECT a FROM parent_values", &[])
                .unwrap()
                .rows
                .len(),
            1
        );
        exec(&engine, "TRUNCATE parent_values");
        assert!(engine
            .sql("SELECT a FROM parent_values", &[])
            .unwrap()
            .rows
            .is_empty());
        exec(
            &engine,
            "INSERT INTO child_values (a, label) VALUES (8, 'child')",
        );
    }

    let reopened = Engine::open(&path).unwrap();
    let rows = reopened.sql("SELECT a, b FROM parent_values", &[]).unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0]["b"], Value::Int(9));
    exec(
        &reopened,
        "ALTER TABLE parent_values RENAME TO renamed_parent_values",
    );
    assert_eq!(
        reopened
            .sql("SELECT b FROM renamed_parent_values", &[])
            .unwrap()
            .rows[0]["b"],
        Value::Int(9)
    );
    let restrict = reopened
        .sql("DROP TABLE renamed_parent_values", &[])
        .unwrap_err();
    assert_eq!(restrict.sqlstate(), Some("2BP01"));
    exec(&reopened, "DROP TABLE renamed_parent_values CASCADE");
    assert!(!reopened.has_table("renamed_parent_values").unwrap());
    assert!(!reopened.has_table("child_values").unwrap());
}

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

fn create_hierarchy_foreign_key_fixture(engine: &Engine) {
    exec(
        engine,
        "CREATE TABLE hierarchy_accounts (region INTEGER, account_id INTEGER, PRIMARY KEY (region, account_id)) PARTITION BY RANGE (region)",
    );
    exec(
        engine,
        "CREATE TABLE hierarchy_accounts_low PARTITION OF hierarchy_accounts FOR VALUES FROM (MINVALUE) TO (10)",
    );
    exec(
        engine,
        "CREATE TABLE hierarchy_accounts_high PARTITION OF hierarchy_accounts FOR VALUES FROM (10) TO (MAXVALUE)",
    );
    exec(
        engine,
        "CREATE TABLE hierarchy_cascade_refs (region INTEGER, account_id INTEGER, marker TEXT, FOREIGN KEY (region, account_id) REFERENCES hierarchy_accounts (region, account_id) ON UPDATE CASCADE ON DELETE CASCADE) PARTITION BY RANGE (region)",
    );
    exec(
        engine,
        "CREATE TABLE hierarchy_cascade_refs_low PARTITION OF hierarchy_cascade_refs FOR VALUES FROM (MINVALUE) TO (10)",
    );
    exec(
        engine,
        "CREATE TABLE hierarchy_cascade_refs_high PARTITION OF hierarchy_cascade_refs FOR VALUES FROM (10) TO (MAXVALUE)",
    );
    exec(
        engine,
        "CREATE TABLE hierarchy_set_refs (region INTEGER, account_id INTEGER, marker TEXT, FOREIGN KEY (region, account_id) REFERENCES hierarchy_accounts (region, account_id) ON DELETE SET NULL) PARTITION BY RANGE (region)",
    );
    exec(
        engine,
        "CREATE TABLE hierarchy_set_refs_low PARTITION OF hierarchy_set_refs FOR VALUES FROM (MINVALUE) TO (10)",
    );
    exec(
        engine,
        "CREATE TABLE hierarchy_set_refs_high PARTITION OF hierarchy_set_refs FOR VALUES FROM (10) TO (MAXVALUE)",
    );
    exec(
        engine,
        "CREATE TABLE hierarchy_set_refs_default PARTITION OF hierarchy_set_refs DEFAULT",
    );
    exec(
        engine,
        "CREATE TABLE hierarchy_restrict_refs (region INTEGER, account_id INTEGER, FOREIGN KEY (region, account_id) REFERENCES hierarchy_accounts (region, account_id))",
    );
}

#[test]
fn hierarchy_foreign_keys_follow_physical_rows_and_route_referential_actions() {
    let engine = Engine::new();
    create_hierarchy_foreign_key_fixture(&engine);

    exec(
        &engine,
        "INSERT INTO hierarchy_accounts VALUES (1, 7), (11, 7)",
    );
    exec(
        &engine,
        "INSERT INTO hierarchy_cascade_refs VALUES (1, 7, 'cascade-low'), (11, 7, 'cascade-high')",
    );
    exec(
        &engine,
        "INSERT INTO hierarchy_set_refs VALUES (11, 7, 'set-high')",
    );
    exec(&engine, "INSERT INTO hierarchy_restrict_refs VALUES (1, 7)");
    let missing_parent = engine
        .sql(
            "INSERT INTO hierarchy_cascade_refs VALUES (2, 99, 'missing')",
            &[],
        )
        .unwrap_err();
    assert!(missing_parent.to_string().contains("FOREIGN KEY"));
    let restricted = engine
        .sql(
            "DELETE FROM hierarchy_accounts WHERE region = 1 AND account_id = 7",
            &[],
        )
        .unwrap_err();
    assert!(restricted.to_string().contains("FOREIGN KEY"));
    assert_eq!(
        engine
            .sql(
                "SELECT account_id FROM hierarchy_accounts WHERE region = 1",
                &[],
            )
            .unwrap()
            .rows
            .len(),
        1
    );
    exec(&engine, "DELETE FROM hierarchy_restrict_refs");

    exec(
        &engine,
        "UPDATE hierarchy_accounts SET region = 12 WHERE region = 1 AND account_id = 7",
    );
    assert!(engine
        .sql("SELECT * FROM hierarchy_accounts_low", &[])
        .unwrap()
        .rows
        .is_empty());
    let cascaded = engine
        .sql(
            "SELECT region, account_id, marker FROM hierarchy_cascade_refs_high ORDER BY marker",
            &[],
        )
        .unwrap();
    assert_eq!(cascaded.rows.len(), 2);
    assert_eq!(cascaded.rows[0]["region"], Value::Int(11));
    assert_eq!(cascaded.rows[1]["region"], Value::Int(12));

    exec(
        &engine,
        "DELETE FROM hierarchy_accounts WHERE region = 12 AND account_id = 7",
    );
    assert_eq!(
        engine
            .sql("SELECT marker FROM hierarchy_cascade_refs", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
    exec(
        &engine,
        "DELETE FROM hierarchy_accounts WHERE region = 11 AND account_id = 7",
    );
    assert!(engine
        .sql("SELECT * FROM hierarchy_cascade_refs", &[])
        .unwrap()
        .rows
        .is_empty());
    let set_row = engine
        .sql(
            "SELECT region, account_id, marker FROM hierarchy_set_refs_default",
            &[],
        )
        .unwrap();
    assert_eq!(set_row.rows.len(), 1);
    assert_eq!(set_row.rows[0]["region"], Value::Null);
    assert_eq!(set_row.rows[0]["account_id"], Value::Null);
    assert_eq!(set_row.rows[0]["marker"], Value::Str("set-high".into()));
}

#[test]
fn partitioned_on_conflict_uses_physical_identity_and_rejects_row_movement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("partition-conflict.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE partition_conflicts (bucket INTEGER, item_key INTEGER, value INTEGER, UNIQUE (bucket, item_key)) PARTITION BY RANGE (bucket)",
        );
        exec(
            &engine,
            "CREATE TABLE partition_conflicts_low PARTITION OF partition_conflicts FOR VALUES FROM (MINVALUE) TO (10)",
        );
        exec(
            &engine,
            "CREATE TABLE partition_conflicts_high PARTITION OF partition_conflicts FOR VALUES FROM (10) TO (MAXVALUE)",
        );
        exec(
            &engine,
            "INSERT INTO partition_conflicts VALUES (1, 5, 10), (11, 5, 20)",
        );
        let low_doc_id = engine
            .sql("SELECT _doc_id FROM partition_conflicts_low", &[])
            .unwrap()
            .rows[0]["_doc_id"]
            .clone();
        let high_doc_id = engine
            .sql("SELECT _doc_id FROM partition_conflicts_high", &[])
            .unwrap()
            .rows[0]["_doc_id"]
            .clone();
        assert_eq!(low_doc_id, high_doc_id);

        let updated = engine
            .sql(
                "INSERT INTO partition_conflicts VALUES (1, 5, 101), (11, 5, 202) ON CONFLICT (bucket, item_key) DO UPDATE SET value = EXCLUDED.value RETURNING bucket, value",
                &[],
            )
            .unwrap();
        assert_eq!(updated.rows.len(), 2);
        assert_eq!(updated.rows[0]["bucket"], Value::Int(1));
        assert_eq!(updated.rows[0]["value"], Value::Int(101));
        assert_eq!(updated.rows[1]["bucket"], Value::Int(11));
        assert_eq!(updated.rows[1]["value"], Value::Int(202));

        let movement = engine
            .sql(
                "INSERT INTO partition_conflicts VALUES (1, 5, 999) ON CONFLICT (bucket, item_key) DO UPDATE SET bucket = 12, value = EXCLUDED.value",
                &[],
            )
            .unwrap_err();
        assert_eq!(movement.sqlstate(), Some("0A000"));
        let rows = engine
            .sql(
                "SELECT bucket, item_key, value FROM partition_conflicts ORDER BY bucket",
                &[],
            )
            .unwrap();
        assert_eq!(rows.rows.len(), 2);
        assert_eq!(rows.rows[0]["bucket"], Value::Int(1));
        assert_eq!(rows.rows[0]["value"], Value::Int(101));
        assert_eq!(rows.rows[1]["bucket"], Value::Int(11));
        assert_eq!(rows.rows[1]["value"], Value::Int(202));
    }

    let reopened = Engine::open(&path).unwrap();
    let rows = reopened
        .sql(
            "SELECT bucket, item_key, value FROM partition_conflicts ORDER BY bucket",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 2);
    assert_eq!(rows.rows[0]["value"], Value::Int(101));
    assert_eq!(rows.rows[1]["value"], Value::Int(202));
}
