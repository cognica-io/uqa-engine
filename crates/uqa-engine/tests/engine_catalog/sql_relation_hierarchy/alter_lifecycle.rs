//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{exec, Engine, Value};

fn strip_constraint_object_ids(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(values) => {
            values.iter_mut().map(strip_constraint_object_ids).sum()
        }
        serde_json::Value::Object(values) => {
            let removed = usize::from(values.remove("object_id").is_some());
            removed
                + values
                    .values_mut()
                    .map(strip_constraint_object_ids)
                    .sum::<usize>()
        }
        _ => 0,
    }
}

#[test]
fn ordinary_inheritance_edges_are_atomic_ordered_rename_safe_and_durable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("alter-inheritance.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE p1 (a INTEGER NOT NULL, CONSTRAINT shared_positive CHECK (a > 0))",
        );
        exec(&engine, "CREATE TABLE p2 (a INTEGER NOT NULL)");
        exec(&engine, "CREATE TABLE child (a INTEGER NOT NULL, extra TEXT, CONSTRAINT shared_positive CHECK (a > 0))");
        exec(&engine, "INSERT INTO child VALUES (1, 'kept')");
        exec(&engine, "ALTER TABLE child INHERIT p1");
        exec(&engine, "ALTER TABLE child INHERIT p2");
        assert_eq!(engine.sql("SELECT a FROM p1", &[]).unwrap().rows.len(), 1);
        exec(&engine, "ALTER TABLE child NO INHERIT p1");
        let edge = engine
            .sql(
                "SELECT parent.relname AS parent, i.inhseqno FROM pg_catalog.pg_inherits AS i JOIN pg_catalog.pg_class AS child_class ON child_class.oid = i.inhrelid JOIN pg_catalog.pg_class AS parent ON parent.oid = i.inhparent WHERE child_class.relname = 'child'",
                &[],
            )
            .unwrap();
        assert_eq!(edge.rows.len(), 1);
        assert_eq!(edge.rows[0]["parent"], Value::Str("p2".into()));
        assert_eq!(edge.rows[0]["inhseqno"], Value::Int(2));
        exec(&engine, "ALTER TABLE p2 RENAME TO renamed_p2");
    }
    let reopened = Engine::open(&path).unwrap();
    assert_eq!(
        reopened
            .sql("SELECT a FROM renamed_p2", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
    exec(&reopened, "BEGIN");
    exec(&reopened, "ALTER TABLE child NO INHERIT renamed_p2");
    exec(&reopened, "ROLLBACK");
    assert_eq!(
        reopened
            .sql("SELECT a FROM renamed_p2", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
}

#[test]
fn inherit_validates_columns_checks_generated_kinds_and_cycles() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE parent_missing (a INTEGER, b TEXT)");
    exec(&engine, "CREATE TABLE child_missing (a INTEGER)");
    assert_eq!(
        engine
            .sql("ALTER TABLE child_missing INHERIT parent_missing", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42804")
    );
    exec(
        &engine,
        "CREATE TABLE parent_check (a INTEGER, CONSTRAINT same_check CHECK (a > 0))",
    );
    exec(
        &engine,
        "CREATE TABLE child_check (a INTEGER, CONSTRAINT same_check CHECK (a >= 0))",
    );
    assert_eq!(
        engine
            .sql("ALTER TABLE child_check INHERIT parent_check", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42804")
    );
    exec(
        &engine,
        "CREATE TABLE parent_generated (a INTEGER, b INTEGER GENERATED ALWAYS AS (a + 1) STORED)",
    );
    exec(
        &engine,
        "CREATE TABLE child_generated (a INTEGER, b INTEGER)",
    );
    assert_eq!(
        engine
            .sql("ALTER TABLE child_generated INHERIT parent_generated", &[],)
            .unwrap_err()
            .sqlstate(),
        Some("42804")
    );
    exec(&engine, "CREATE TABLE cycle_parent (a INTEGER)");
    exec(&engine, "CREATE TABLE cycle_child (a INTEGER)");
    exec(&engine, "ALTER TABLE cycle_child INHERIT cycle_parent");
    assert_eq!(
        engine
            .sql("ALTER TABLE cycle_parent INHERIT cycle_child", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42P07")
    );
}

#[test]
fn local_column_redeclaration_without_default_retains_inherited_default() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE default_parent (value INTEGER DEFAULT 7)",
    );
    exec(
        &engine,
        "CREATE TABLE default_child (value INTEGER) INHERITS (default_parent)",
    );
    exec(
        &engine,
        "INSERT INTO default_child (value) VALUES (DEFAULT)",
    );
    assert_eq!(
        engine
            .sql("SELECT value FROM ONLY default_child", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(7)
    );
}

#[test]
fn alter_table_recurses_columns_checks_and_not_null_but_honors_only() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE alter_parent (a INTEGER)");
    exec(
        &engine,
        "CREATE TABLE alter_child () INHERITS (alter_parent)",
    );

    let only_column = engine
        .sql(
            "ALTER TABLE ONLY alter_parent ADD COLUMN blocked INTEGER",
            &[],
        )
        .unwrap_err();
    assert_eq!(only_column.sqlstate(), Some("42P16"));
    let only_check = engine
        .sql(
            "ALTER TABLE ONLY alter_parent ADD CONSTRAINT blocked_check CHECK (a > 0)",
            &[],
        )
        .unwrap_err();
    assert_eq!(only_check.sqlstate(), Some("42P16"));

    exec(
        &engine,
        "ALTER TABLE alter_parent ADD COLUMN inherited INTEGER DEFAULT 7",
    );
    exec(
        &engine,
        "ALTER TABLE alter_parent ADD CONSTRAINT inherited_check CHECK (a > 0)",
    );
    let invalid_child = engine
        .sql("INSERT INTO alter_child (a) VALUES (-1)", &[])
        .unwrap_err();
    assert_eq!(invalid_child.sqlstate(), Some("23514"));
    exec(&engine, "INSERT INTO alter_child (a) VALUES (1)");
    assert_eq!(
        engine
            .sql("SELECT inherited FROM ONLY alter_child", &[])
            .unwrap()
            .rows[0]["inherited"],
        Value::Int(7)
    );

    exec(
        &engine,
        "ALTER TABLE ONLY alter_parent ALTER COLUMN a SET NOT NULL",
    );
    exec(&engine, "INSERT INTO alter_child (a) VALUES (NULL)");
    exec(&engine, "DELETE FROM alter_child WHERE a IS NULL");
    exec(
        &engine,
        "ALTER TABLE alter_parent ALTER COLUMN a SET NOT NULL",
    );
    assert_eq!(
        engine
            .sql("INSERT INTO alter_child (a) VALUES (NULL)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("23502")
    );

    exec(&engine, "CREATE SCHEMA \"alter.dot\"");
    exec(
        &engine,
        "CREATE TABLE \"alter.dot\".\"parent.dot\" (a INTEGER)",
    );
    exec(
        &engine,
        "CREATE TABLE \"alter.dot\".\"child.dot\" () INHERITS (\"alter.dot\".\"parent.dot\")",
    );
    exec(&engine, "ALTER TABLE \"alter.dot\".\"parent.dot\" ADD COLUMN generated_value INTEGER GENERATED ALWAYS AS (a + 1) STORED");
    exec(
        &engine,
        "INSERT INTO \"alter.dot\".\"child.dot\" (a) VALUES (4)",
    );
    assert_eq!(
        engine
            .sql(
                "SELECT generated_value FROM ONLY \"alter.dot\".\"child.dot\"",
                &[],
            )
            .unwrap()
            .rows[0]["generated_value"],
        Value::Int(5)
    );
}

#[test]
fn attach_partition_validates_existing_and_default_rows_then_routes_immediately() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE routed (k INTEGER) PARTITION BY RANGE (k)",
    );
    exec(
        &engine,
        "CREATE TABLE routed_default PARTITION OF routed DEFAULT",
    );
    exec(&engine, "INSERT INTO routed_default VALUES (5)");
    exec(&engine, "CREATE TABLE routed_low (k INTEGER)");
    let default_conflict = engine
        .sql(
            "ALTER TABLE routed ATTACH PARTITION routed_low FOR VALUES FROM (0) TO (10)",
            &[],
        )
        .unwrap_err();
    assert_eq!(default_conflict.sqlstate(), Some("23514"));
    assert!(engine
        .sql("SELECT * FROM ONLY routed_low", &[])
        .unwrap()
        .rows
        .is_empty());
    exec(&engine, "UPDATE routed_default SET k = 20");
    exec(
        &engine,
        "ALTER TABLE routed ATTACH PARTITION routed_low FOR VALUES FROM (0) TO (10)",
    );
    exec(&engine, "INSERT INTO routed VALUES (1)");
    assert_eq!(
        engine.sql("SELECT k FROM routed_low", &[]).unwrap().rows[0]["k"],
        Value::Int(1)
    );
    exec(&engine, "CREATE TABLE bad_candidate (k INTEGER)");
    exec(&engine, "INSERT INTO bad_candidate VALUES (35)");
    assert_eq!(
        engine
            .sql(
                "ALTER TABLE routed ATTACH PARTITION bad_candidate FOR VALUES FROM (30) TO (35)",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );
}

#[test]
fn attach_scans_partitioned_candidate_descendants_and_requires_an_exact_row_type() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE scan_parent (k INTEGER, subkey INTEGER) PARTITION BY RANGE (k)",
    );
    exec(
        &engine,
        "CREATE TABLE scan_candidate (k INTEGER, subkey INTEGER) PARTITION BY RANGE (subkey)",
    );
    exec(
        &engine,
        "CREATE TABLE scan_leaf PARTITION OF scan_candidate FOR VALUES FROM (0) TO (10)",
    );
    exec(&engine, "INSERT INTO scan_candidate VALUES (15, 1)");
    assert_eq!(
        engine
            .sql(
                "ALTER TABLE scan_parent ATTACH PARTITION scan_candidate FOR VALUES FROM (10) TO (15)",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );

    exec(
        &engine,
        "CREATE TABLE extra_candidate (k INTEGER, subkey INTEGER, extra INTEGER)",
    );
    assert_eq!(
        engine
            .sql(
                "ALTER TABLE scan_parent ATTACH PARTITION extra_candidate FOR VALUES FROM (20) TO (30)",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("42804")
    );

    exec(
        &engine,
        "CREATE TABLE identity_candidate (k INTEGER GENERATED ALWAYS AS IDENTITY, subkey INTEGER)",
    );
    assert_eq!(
        engine
            .sql(
                "ALTER TABLE scan_parent ATTACH PARTITION identity_candidate FOR VALUES FROM (30) TO (40)",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("55000")
    );
}

#[test]
fn attach_propagates_identity_key_and_foreign_key_then_detach_localizes_schema() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE referenced (id INTEGER PRIMARY KEY)");
    exec(&engine, "INSERT INTO referenced VALUES (1)");
    exec(&engine, "CREATE TABLE parent_identity (id INTEGER GENERATED ALWAYS AS IDENTITY, k INTEGER NOT NULL, ref_id INTEGER REFERENCES referenced(id), generated_value INTEGER GENERATED ALWAYS AS (k + 1) STORED, CONSTRAINT parent_identity_check CHECK (k >= 0), PRIMARY KEY (k, id)) PARTITION BY RANGE (k)");
    exec(&engine, "CREATE TABLE attached_identity (id INTEGER NOT NULL, k INTEGER NOT NULL, ref_id INTEGER, generated_value INTEGER GENERATED ALWAYS AS (k + 2) STORED, CONSTRAINT parent_identity_check CHECK (k >= 0))");
    exec(
        &engine,
        "INSERT INTO attached_identity (id, k, ref_id) VALUES (20, 2, 1)",
    );
    exec(&engine, "ALTER TABLE parent_identity ATTACH PARTITION attached_identity FOR VALUES FROM (0) TO (10)");
    let inserted = engine
        .sql(
            "INSERT INTO parent_identity (k, ref_id) VALUES (3, 1) RETURNING id",
            &[],
        )
        .unwrap();
    assert_eq!(inserted.rows[0]["id"], Value::Int(1));
    let missing_reference = engine
        .sql(
            "INSERT INTO attached_identity (k, ref_id) VALUES (4, 999)",
            &[],
        )
        .unwrap_err();
    assert_eq!(missing_reference.sqlstate(), Some("23503"));
    assert!(missing_reference
        .to_string()
        .contains("parent_identity_ref_id_fkey"));
    exec(
        &engine,
        "ALTER TABLE parent_identity DETACH PARTITION attached_identity",
    );
    assert!(engine
        .sql("SELECT * FROM ONLY parent_identity", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        engine
            .sql(
                "INSERT INTO attached_identity (k, ref_id) VALUES (5, 1)",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("23502")
    );
    exec(
        &engine,
        "INSERT INTO attached_identity (id, k, ref_id) VALUES (21, 6, 999)",
    );
}

#[test]
fn detach_removes_only_constraints_copied_by_attach() {
    let engine = Engine::new();
    exec(&engine, "CREATE TABLE detach_ref (id INTEGER PRIMARY KEY)");
    exec(&engine, "INSERT INTO detach_ref VALUES (1)");
    exec(&engine, "CREATE TABLE detach_parent (k INTEGER PRIMARY KEY, ref_id INTEGER REFERENCES detach_ref(id)) PARTITION BY RANGE (k)");
    exec(&engine, "CREATE TABLE detach_child (k INTEGER PRIMARY KEY, ref_id INTEGER REFERENCES detach_ref(id))");
    exec(
        &engine,
        "ALTER TABLE detach_parent ATTACH PARTITION detach_child FOR VALUES FROM (0) TO (10)",
    );
    exec(
        &engine,
        "ALTER TABLE detach_parent DETACH PARTITION detach_child",
    );
    exec(&engine, "INSERT INTO detach_child VALUES (1, 1)");
    let duplicate = engine
        .sql("INSERT INTO detach_child VALUES (1, 1)", &[])
        .unwrap_err();
    assert_eq!(duplicate.sqlstate(), Some("23505"), "{duplicate}");
    let missing_reference = engine
        .sql("INSERT INTO detach_child VALUES (2, 999)", &[])
        .unwrap_err();
    assert_eq!(
        missing_reference.sqlstate(),
        Some("23503"),
        "{missing_reference}"
    );
}

#[test]
fn attached_identity_temporarily_overrides_and_detach_restores_a_serial_generator() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE serial_parent (id INTEGER GENERATED ALWAYS AS IDENTITY, k INTEGER) PARTITION BY RANGE (k)",
    );
    exec(&engine, "CREATE TABLE serial_child (id SERIAL, k INTEGER)");
    let seeded = engine
        .sql("INSERT INTO serial_child (k) VALUES (1) RETURNING id", &[])
        .unwrap();
    assert_eq!(seeded.rows[0]["id"], Value::Int(1));
    exec(&engine, "DELETE FROM serial_child");
    exec(
        &engine,
        "ALTER TABLE serial_parent ATTACH PARTITION serial_child FOR VALUES FROM (0) TO (10)",
    );
    let attached = engine
        .sql("INSERT INTO serial_child (k) VALUES (2) RETURNING id", &[])
        .unwrap();
    assert_eq!(attached.rows[0]["id"], Value::Int(1));
    exec(
        &engine,
        "ALTER TABLE serial_parent DETACH PARTITION serial_child",
    );
    let detached = engine
        .sql("INSERT INTO serial_child (k) VALUES (3) RETURNING id", &[])
        .unwrap();
    assert_eq!(detached.rows[0]["id"], Value::Int(2));
}

#[test]
fn attach_and_detach_edges_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("alter-partition.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE durable_parent (k INTEGER) PARTITION BY RANGE (k)",
        );
        exec(&engine, "CREATE TABLE durable_child (k INTEGER)");
        exec(&engine, "INSERT INTO durable_child VALUES (1)");
        exec(
            &engine,
            "ALTER TABLE durable_parent ATTACH PARTITION durable_child FOR VALUES FROM (0) TO (10)",
        );
    }
    {
        let engine = Engine::open(&path).unwrap();
        assert_eq!(
            engine
                .sql("SELECT * FROM durable_parent", &[])
                .unwrap()
                .rows
                .len(),
            1
        );
        exec(
            &engine,
            "ALTER TABLE durable_parent DETACH PARTITION durable_child",
        );
    }
    let reopened = Engine::open(&path).unwrap();
    assert!(reopened
        .sql("SELECT * FROM durable_parent", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        reopened
            .sql("SELECT * FROM durable_child", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
}

#[test]
fn legacy_partition_foreign_key_ids_are_synchronized_before_detach() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-partition-foreign-key.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(
            &engine,
            "CREATE TABLE legacy_reference (id INTEGER PRIMARY KEY)",
        );
        exec(&engine, "CREATE TABLE legacy_parent (k INTEGER, ref_id INTEGER CONSTRAINT legacy_parent_fk REFERENCES legacy_reference(id)) PARTITION BY RANGE (k)");
        exec(
            &engine,
            "CREATE TABLE legacy_child (k INTEGER, ref_id INTEGER)",
        );
        exec(
            &engine,
            "ALTER TABLE legacy_parent ATTACH PARTITION legacy_child FOR VALUES FROM (0) TO (10)",
        );
    }
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        let mut statement = connection
            .prepare("SELECT schema_name, relation_name, columns, constraints FROM _tables")
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        drop(statement);
        let mut removed = 0;
        for (schema, relation, columns, constraints) in rows {
            let mut columns: serde_json::Value = serde_json::from_str(&columns).unwrap();
            let mut constraints: serde_json::Value = serde_json::from_str(&constraints).unwrap();
            removed += strip_constraint_object_ids(&mut columns);
            removed += strip_constraint_object_ids(&mut constraints);
            connection
                .execute(
                    "UPDATE _tables SET columns = ?1, constraints = ?2 WHERE schema_name = ?3 AND relation_name = ?4",
                    rusqlite::params![serde_json::to_string(&columns).unwrap(), serde_json::to_string(&constraints).unwrap(), schema, relation],
                )
                .unwrap();
        }
        assert!(
            removed >= 3,
            "expected parent, live child, and provenance IDs"
        );
    }

    let reopened = Engine::open(&path).unwrap();
    exec(
        &reopened,
        "ALTER TABLE legacy_parent DETACH PARTITION legacy_child",
    );
    exec(&reopened, "INSERT INTO legacy_child VALUES (1, 999)");
}

#[test]
fn reopen_repairs_a_legacy_dangling_hierarchy_parent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dangling-parent.db");
    {
        let engine = Engine::open(&path).unwrap();
        exec(&engine, "CREATE TABLE vanished_parent (a INTEGER)");
        exec(
            &engine,
            "CREATE TABLE surviving_child () INHERITS (vanished_parent)",
        );
        exec(&engine, "INSERT INTO surviving_child VALUES (1)");
    }
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute(
                "DELETE FROM _tables WHERE schema_name = 'public' AND relation_name = 'vanished_parent'",
                [],
            )
            .unwrap();
        connection
            .execute(
                "DELETE FROM _relations WHERE schema_name = 'public' AND relation_name = 'vanished_parent'",
                [],
            )
            .unwrap();
    }
    let reopened = Engine::open(&path).unwrap();
    assert!(reopened
        .sql(
            "SELECT * FROM pg_catalog.pg_inherits AS edge JOIN pg_catalog.pg_class AS child ON child.oid = edge.inhrelid WHERE child.relname = 'surviving_child'",
            &[],
        )
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        reopened
            .sql("SELECT a FROM surviving_child", &[])
            .unwrap()
            .rows[0]["a"],
        Value::Int(1)
    );
    reopened.column_stats("surviving_child").unwrap();
}

#[test]
fn detach_concurrently_enforces_transaction_default_finalize_and_retained_bound_rules() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE concurrent_parent (k INTEGER) PARTITION BY RANGE (k)",
    );
    exec(
        &engine,
        "CREATE TABLE concurrent_child PARTITION OF concurrent_parent FOR VALUES FROM (0) TO (10)",
    );
    exec(&engine, "BEGIN");
    assert_eq!(
        engine
            .sql(
                "ALTER TABLE concurrent_parent DETACH PARTITION concurrent_child CONCURRENTLY",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("25001")
    );
    exec(&engine, "ROLLBACK");
    assert_eq!(
        engine
            .sql(
                "ALTER TABLE concurrent_parent DETACH PARTITION concurrent_child CONCURRENTLY; SELECT 1",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("25001")
    );
    exec(
        &engine,
        "ALTER TABLE concurrent_parent DETACH PARTITION concurrent_child CONCURRENTLY",
    );
    assert_eq!(
        engine
            .sql("INSERT INTO concurrent_child VALUES (11)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );
    assert!(engine
        .sql("SELECT * FROM concurrent_parent", &[])
        .unwrap()
        .rows
        .is_empty());

    exec(
        &engine,
        "CREATE TABLE default_parent (k INTEGER) PARTITION BY RANGE (k)",
    );
    exec(
        &engine,
        "CREATE TABLE default_child PARTITION OF default_parent FOR VALUES FROM (0) TO (10)",
    );
    exec(
        &engine,
        "CREATE TABLE default_fallback PARTITION OF default_parent DEFAULT",
    );
    assert_eq!(
        engine
            .sql(
                "ALTER TABLE default_parent DETACH PARTITION default_child CONCURRENTLY",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("55000")
    );
    assert_eq!(
        engine
            .sql(
                "ALTER TABLE default_parent DETACH PARTITION default_child FINALIZE",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("55000")
    );
}

#[test]
fn attach_and_detach_follow_explicit_transaction_rollback() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE tx_parent (k INTEGER) PARTITION BY RANGE (k)",
    );
    exec(&engine, "CREATE TABLE tx_child (k INTEGER)");
    exec(&engine, "INSERT INTO tx_child VALUES (1)");
    exec(&engine, "BEGIN");
    exec(
        &engine,
        "ALTER TABLE tx_parent ATTACH PARTITION tx_child FOR VALUES FROM (0) TO (10)",
    );
    assert_eq!(
        engine
            .sql("SELECT * FROM tx_parent", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
    exec(&engine, "ROLLBACK");
    assert!(engine
        .sql("SELECT * FROM tx_parent", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        engine
            .sql("SELECT * FROM tx_child", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
}
