//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-key and ON CONFLICT behavior over physical hierarchy rows.

use super::{exec, Engine, Value};

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
    assert_eq!(missing_parent.sqlstate(), Some("23503"));
    assert!(missing_parent
        .to_string()
        .contains("foreign key constraint"));
    let restricted = engine
        .sql(
            "DELETE FROM hierarchy_accounts WHERE region = 1 AND account_id = 7",
            &[],
        )
        .unwrap_err();
    assert_eq!(restricted.sqlstate(), Some("23503"));
    assert!(restricted.to_string().contains("foreign key constraint"));
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
fn hierarchy_temporal_cross_type_foreign_keys_scan_physical_partitions() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE hierarchy_periods (
            bucket SMALLINT,
            tenant INTEGER,
            valid_at DATERANGE,
            CONSTRAINT hierarchy_periods_pk PRIMARY KEY (bucket, tenant, valid_at WITHOUT OVERLAPS)
        ) PARTITION BY RANGE (bucket)",
    );
    exec(
        &engine,
        "CREATE TABLE hierarchy_periods_low PARTITION OF hierarchy_periods FOR VALUES FROM (MINVALUE) TO (10)",
    );
    exec(
        &engine,
        "CREATE TABLE hierarchy_periods_high PARTITION OF hierarchy_periods FOR VALUES FROM (10) TO (MAXVALUE)",
    );
    exec(
        &engine,
        "CREATE TABLE hierarchy_period_refs (
            row_id INTEGER PRIMARY KEY,
            bucket BIGINT,
            tenant BIGINT,
            valid_at DATERANGE,
            CONSTRAINT hierarchy_period_refs_fk FOREIGN KEY (bucket, tenant, PERIOD valid_at)
                REFERENCES hierarchy_periods (bucket, tenant, PERIOD valid_at)
        )",
    );
    exec(
        &engine,
        "INSERT INTO hierarchy_periods VALUES
            (1, 7, '[2024-01-01,2024-01-10)'),
            (1, 7, '[2024-01-10,2024-01-20)'),
            (11, 8, '[2024-02-01,2024-02-10)'),
            (11, 8, '[2024-02-10,2024-02-20)')",
    );
    exec(
        &engine,
        "INSERT INTO hierarchy_period_refs VALUES
            (1, 1, 7, '[2024-01-05,2024-01-15)'),
            (2, 11, 8, '[2024-02-05,2024-02-15)')",
    );

    let uncovered = engine
        .sql(
            "INSERT INTO hierarchy_period_refs VALUES (3, 11, 8, '[2024-02-05,2024-02-25)')",
            &[],
        )
        .unwrap_err();
    assert_eq!(uncovered.sqlstate(), Some("23503"));
    assert!(uncovered.to_string().contains("hierarchy_period_refs_fk"));

    let referenced_delete = engine
        .sql(
            "DELETE FROM hierarchy_periods WHERE bucket = 11 AND valid_at = '[2024-02-01,2024-02-10)'",
            &[],
        )
        .unwrap_err();
    assert_eq!(referenced_delete.sqlstate(), Some("23503"));
    assert!(referenced_delete
        .to_string()
        .contains("hierarchy_period_refs_fk"));
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
