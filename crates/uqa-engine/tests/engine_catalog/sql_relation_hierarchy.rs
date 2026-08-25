//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 table inheritance and declarative partition routing.

use uqa_core::Value;
use uqa_engine::Engine;

#[path = "sql_relation_hierarchy/fk_conflict.rs"]
mod fk_conflict;
#[path = "sql_relation_hierarchy/hash.rs"]
mod hash;
#[path = "sql_relation_hierarchy/merge.rs"]
mod merge;
#[path = "sql_relation_hierarchy/movement.rs"]
mod movement;
#[path = "sql_relation_hierarchy/regressions.rs"]
mod regressions;
#[path = "sql_relation_hierarchy/retrieval.rs"]
mod retrieval;

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
