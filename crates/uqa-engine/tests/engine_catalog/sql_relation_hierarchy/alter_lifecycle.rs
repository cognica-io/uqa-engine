//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{exec, Engine, Value};

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
