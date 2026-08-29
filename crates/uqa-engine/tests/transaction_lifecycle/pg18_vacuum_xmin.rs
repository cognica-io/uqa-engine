//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn assert_pg18_vacuum_validation_errors(eng: &Engine) {
    for (sql, message) in [
        (
            "VACUUM (FULL, PARALLEL 1)",
            "VACUUM FULL cannot be performed in parallel",
        ),
        (
            "VACUUM (ONLY_DATABASE_STATS, ANALYZE)",
            "ONLY_DATABASE_STATS cannot be specified with other VACUUM options",
        ),
        (
            "VACUUM (ONLY_DATABASE_STATS) vacuum_target",
            "ONLY_DATABASE_STATS cannot be specified with a list of tables",
        ),
        (
            "VACUUM (FULL, BUFFER_USAGE_LIMIT '128 kB') vacuum_target",
            "BUFFER_USAGE_LIMIT cannot be specified for VACUUM FULL",
        ),
        (
            "VACUUM (FULL, DISABLE_PAGE_SKIPPING) vacuum_target",
            "VACUUM option DISABLE_PAGE_SKIPPING cannot be used with FULL",
        ),
        (
            "VACUUM (FULL, PROCESS_TOAST FALSE) vacuum_target",
            "PROCESS_TOAST required with VACUUM FULL",
        ),
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
        assert_eq!(error.to_string(), message, "{sql}");
    }

    let unknown_option = eng.sql("VACUUM (NOT_A_PG18_OPTION)", &[]).unwrap_err();
    assert_eq!(unknown_option.sqlstate(), Some("42601"), "{unknown_option}");
    let invalid_buffer = eng
        .sql("VACUUM (BUFFER_USAGE_LIMIT '127 kB')", &[])
        .unwrap_err();
    assert_eq!(invalid_buffer.sqlstate(), Some("22023"), "{invalid_buffer}");
    let invalid_boolean = eng.sql("VACUUM (VERBOSE 2)", &[]).unwrap_err();
    assert_eq!(
        invalid_boolean.sqlstate(),
        Some("42601"),
        "{invalid_boolean}"
    );
    let invalid_boolean_alias = eng.sql("VACUUM (ANALYZE yes)", &[]).unwrap_err();
    assert_eq!(
        invalid_boolean_alias.sqlstate(),
        Some("42601"),
        "{invalid_boolean_alias}"
    );
    let missing_table = eng.sql("VACUUM missing_vacuum_target", &[]).unwrap_err();
    assert_eq!(missing_table.sqlstate(), Some("42P01"), "{missing_table}");
    let columns_without_analyze = eng.sql("VACUUM vacuum_target (a)", &[]).unwrap_err();
    assert_eq!(
        columns_without_analyze.sqlstate(),
        Some("0A000"),
        "{columns_without_analyze}"
    );
    let missing_column = eng
        .sql("VACUUM (ANALYZE) vacuum_target (missing)", &[])
        .unwrap_err();
    assert_eq!(missing_column.sqlstate(), Some("42703"), "{missing_column}");
}
#[test]
fn pg18_vacuum_runs_outside_transactions_and_preserves_error_precedence() {
    let directory = tempfile::tempdir().unwrap();
    let eng = Engine::open(&directory.path().join("vacuum.db")).unwrap();
    eng.sql("CREATE TABLE vacuum_target (a INTEGER, b TEXT)", &[])
        .unwrap();
    eng.sql("INSERT INTO vacuum_target VALUES (1, 'one')", &[])
        .unwrap();
    eng.sql("VACUUM", &[]).unwrap();
    eng.sql(
        "VACUUM (ANALYZE, FULL, FREEZE, PARALLEL 0, BUFFER_USAGE_LIMIT '128 kB') vacuum_target (a)",
        &[],
    )
    .unwrap();
    assert_eq!(
        integer_column(&eng.sql("SELECT a FROM vacuum_target", &[]).unwrap(), "a"),
        [1]
    );
    eng.sql("UPDATE vacuum_target SET b = 'rewritten' WHERE a = 1", &[])
        .unwrap();
    eng.sql("VACUUM (BUFFER_USAGE_LIMIT '0.125 MB')", &[])
        .unwrap();
    eng.sql("VACUUM (ONLY_DATABASE_STATS, ANALYZE false)", &[])
        .unwrap();
    eng.sql("VACUUM (ONLY_DATABASE_STATS, BUFFER_USAGE_LIMIT 0)", &[])
        .unwrap();
    eng.sql(
        "VACUUM (ONLY_DATABASE_STATS, VERBOSE 0, PROCESS_MAIN FALSE, PROCESS_TOAST FALSE, INDEX_CLEANUP AUTO, TRUNCATE FALSE, PARALLEL 2, BUFFER_USAGE_LIMIT '128 kB')",
        &[],
    )
    .unwrap();
    eng.sql("VACUUM (VERBOSE 0) vacuum_target", &[]).unwrap();
    eng.sql("VACUUM (VERBOSE 1) vacuum_target", &[]).unwrap();
    eng.sql("VACUUM (ONLY_DATABASE_STATS false, ANALYZE)", &[])
        .unwrap();
    eng.sql("SET default_transaction_read_only = on", &[])
        .unwrap();
    eng.sql("VACUUM (ANALYZE) vacuum_target", &[]).unwrap();
    eng.sql("RESET default_transaction_read_only", &[]).unwrap();

    eng.sql(
        "CREATE TABLE vacuum_parent (a INTEGER, b TEXT) PARTITION BY RANGE (a); CREATE TABLE vacuum_child PARTITION OF vacuum_parent FOR VALUES FROM (0) TO (10); INSERT INTO vacuum_parent VALUES (1, 'one'), (2, 'two')",
        &[],
    )
    .unwrap();
    eng.sql("VACUUM (ANALYZE) ONLY vacuum_parent (a)", &[])
        .unwrap();
    let parent_only = eng.column_stats("vacuum_parent").unwrap();
    assert_eq!(parent_only["a"].row_count, 0);
    assert!(!parent_only.contains_key("b"));
    eng.sql("VACUUM (ANALYZE) vacuum_parent (b)", &[]).unwrap();
    let descendants = eng.column_stats("vacuum_parent").unwrap();
    assert_eq!(descendants["a"].row_count, 0);
    assert_eq!(descendants["b"].row_count, 2);

    assert_pg18_vacuum_validation_errors(&eng);

    eng.sql("BEGIN", &[]).unwrap();
    let inside = eng.sql("VACUUM (NOT_A_PG18_OPTION)", &[]).unwrap_err();
    assert_eq!(inside.sqlstate(), Some("25001"), "{inside}");
    assert!(inside
        .to_string()
        .contains("VACUUM cannot run inside a transaction block"));
    eng.sql("ROLLBACK", &[]).unwrap();

    let batch = eng.sql("VACUUM; SELECT 1", &[]).unwrap_err();
    assert_eq!(batch.sqlstate(), Some("25001"), "{batch}");
    assert_eq!(eng.transaction_depth(), 0);
}

#[test]
fn pg18_vacuum_full_waits_for_relation_holders_before_reserving_the_backend_writer() {
    let directory = tempfile::tempdir().unwrap();
    let root =
        std::sync::Arc::new(Engine::open(&directory.path().join("vacuum-locks.db")).unwrap());
    root.sql(
        "CREATE TABLE vacuum_lock_rows (id INTEGER PRIMARY KEY, value INTEGER); INSERT INTO vacuum_lock_rows VALUES (1, 10)",
        &[],
    )
    .unwrap();
    let vacuum = std::sync::Arc::new(root.new_session().unwrap());
    root.sql("BEGIN", &[]).unwrap();
    root.sql("SELECT * FROM vacuum_lock_rows FOR UPDATE", &[])
        .unwrap();

    let (sender, receiver) = std::sync::mpsc::channel();
    let vacuum_thread = std::thread::spawn(move || {
        sender
            .send(vacuum.sql("VACUUM FULL vacuum_lock_rows", &[]))
            .unwrap();
    });
    std::thread::sleep(std::time::Duration::from_millis(100));
    root.sql("UPDATE vacuum_lock_rows SET value = 20 WHERE id = 1", &[])
        .unwrap();
    assert!(receiver.try_recv().is_err());
    root.sql("COMMIT", &[]).unwrap();
    receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap()
        .unwrap();
    vacuum_thread.join().unwrap();
    assert_eq!(
        integer_column(
            &root
                .sql("SELECT value FROM vacuum_lock_rows WHERE id = 1", &[])
                .unwrap(),
            "value",
        ),
        [20]
    );
}

#[test]
fn pg18_vacuum_full_reclaims_persistent_file_space() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vacuum-full-size.db");
    {
        let engine = Engine::open(&path).unwrap();
        engine
            .sql(
                "CREATE TABLE vacuum_size_rows (id INTEGER PRIMARY KEY, value TEXT); INSERT INTO vacuum_size_rows SELECT x, repeat('x', 2048) FROM generate_series(1, 2000) AS rows(x); DELETE FROM vacuum_size_rows WHERE id > 10",
                &[],
            )
            .unwrap();
    }
    let size_before = std::fs::metadata(&path).unwrap().len();

    {
        let engine = Engine::open(&path).unwrap();
        engine.sql("VACUUM FULL vacuum_size_rows", &[]).unwrap();
    }
    let size_after = std::fs::metadata(&path).unwrap().len();
    assert!(
        size_after < size_before / 2,
        "VACUUM FULL did not reclaim enough space: before={size_before}, after={size_after}"
    );
}

#[test]
fn pg18_vacuum_full_rewrites_compressed_storage() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vacuum-full-compressed.uqac.sqlite3");
    let engine =
        Engine::open_compressed(&path, uqa_storage::SQLiteCompressionOptions::default()).unwrap();
    engine
        .sql(
            "CREATE TABLE vacuum_compressed_rows (id INTEGER PRIMARY KEY, value TEXT); INSERT INTO vacuum_compressed_rows SELECT x, repeat('x', 1024) FROM generate_series(1, 500) AS rows(x); DELETE FROM vacuum_compressed_rows WHERE id > 2",
            &[],
        )
        .unwrap();
    engine
        .sql("VACUUM FULL vacuum_compressed_rows", &[])
        .unwrap();
    let rows = engine
        .sql("SELECT id FROM vacuum_compressed_rows ORDER BY id", &[])
        .unwrap();
    assert_eq!(integer_column(&rows, "id"), [1, 2]);
}

#[test]
fn pg18_xmin_tracks_top_level_and_savepoint_tuple_versions() {
    let eng = Engine::new();
    for column in ["tableoid", "xmin", "cmin", "xmax", "cmax", "ctid"] {
        let error = eng
            .sql(
                &format!("CREATE TABLE reserved_{column} ({column} TEXT)"),
                &[],
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42701"), "{column}: {error}");
        assert_eq!(
            error.to_string(),
            format!("column name \"{column}\" conflicts with a system column name")
        );
    }
    eng.sql(
        "CREATE TABLE quoted_system_name (\"XMIN\" TEXT, value INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE SERVER system_column_server FOREIGN DATA WRAPPER duckdb_fdw OPTIONS (database ':memory:')",
        &[],
    )
    .unwrap();
    for sql in [
        "CREATE FOREIGN TABLE reserved_foreign_xmin (xmin INTEGER) SERVER system_column_server",
        "CREATE MATERIALIZED VIEW reserved_materialized_xmin AS SELECT 1 AS xmin WITH NO DATA",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42701"), "{sql}: {error}");
    }
    eng.sql(
        "CREATE FOREIGN TABLE quoted_foreign_xmin (\"XMIN\" INTEGER) SERVER system_column_server",
        &[],
    )
    .unwrap();
    eng.sql("CREATE VIEW ordinary_xmin_view AS SELECT 1 AS xmin", &[])
        .unwrap();
    for sql in [
        "ALTER TABLE quoted_system_name ADD COLUMN xmin TEXT",
        "ALTER TABLE quoted_system_name RENAME COLUMN value TO xmin",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42701"), "{sql}: {error}");
    }
    eng.sql("CREATE TABLE xacttest (a INTEGER)", &[]).unwrap();
    eng.add_document(
        "xacttest",
        99,
        Document::from([("a".into(), uqa_core::Value::Int(99))]),
    )
    .unwrap();
    let direct_xmin = integer_column(
        &eng.sql("SELECT xmin FROM xacttest WHERE a = 99", &[])
            .unwrap(),
        "xmin",
    );
    assert_eq!(direct_xmin.len(), 1);
    assert!(direct_xmin[0] > 0);
    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("INSERT INTO xacttest VALUES (6)", &[]).unwrap();
    eng.sql("SAVEPOINT one", &[]).unwrap();
    eng.sql("INSERT INTO xacttest VALUES (7)", &[]).unwrap();
    eng.sql("RELEASE SAVEPOINT one", &[]).unwrap();
    eng.sql("INSERT INTO xacttest VALUES (8)", &[]).unwrap();
    eng.sql("COMMIT", &[]).unwrap();

    let released = eng
        .sql(
            "SELECT a.xmin = b.xmin AS same FROM xacttest a CROSS JOIN xacttest b WHERE a.a = 6 AND b.a = 8",
            &[],
        )
        .unwrap();
    assert_eq!(released.rows[0]["same"], uqa_core::Value::Bool(true));
    let child = eng
        .sql(
            "SELECT a.xmin = b.xmin AS same FROM xacttest a CROSS JOIN xacttest b WHERE a.a = 6 AND b.a = 7",
            &[],
        )
        .unwrap();
    assert_eq!(child.rows[0]["same"], uqa_core::Value::Bool(false));

    eng.sql("BEGIN", &[]).unwrap();
    eng.sql("INSERT INTO xacttest VALUES (9)", &[]).unwrap();
    eng.sql("SAVEPOINT one", &[]).unwrap();
    eng.sql("INSERT INTO xacttest VALUES (10)", &[]).unwrap();
    eng.sql("ROLLBACK TO SAVEPOINT one", &[]).unwrap();
    eng.sql("INSERT INTO xacttest VALUES (11)", &[]).unwrap();
    eng.sql("COMMIT", &[]).unwrap();

    let restarted = eng
        .sql(
            "SELECT a.xmin = b.xmin AS same FROM xacttest a CROSS JOIN xacttest b WHERE a.a = 9 AND b.a = 11",
            &[],
        )
        .unwrap();
    assert_eq!(restarted.rows[0]["same"], uqa_core::Value::Bool(false));
    assert!(eng
        .sql("SELECT a FROM xacttest WHERE a = 10", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn pg18_xmin_is_explicitly_addressable_but_hidden_from_stars_and_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("xmin.db");
    let first_xmin = {
        let eng = Engine::open(&path).unwrap();
        eng.sql("CREATE TABLE versioned (a INTEGER)", &[]).unwrap();
        let inserted = eng
            .sql("INSERT INTO versioned VALUES (1) RETURNING xmin", &[])
            .unwrap();
        assert_eq!(inserted.columns, ["xmin"]);
        let star = eng.sql("SELECT * FROM versioned", &[]).unwrap();
        assert_eq!(star.columns, ["a"]);
        inserted.rows[0]["xmin"].clone()
    };

    let eng = Engine::open(&path).unwrap();
    let inserted = eng
        .sql("INSERT INTO versioned VALUES (2) RETURNING xmin", &[])
        .unwrap();
    assert_ne!(inserted.rows[0]["xmin"], first_xmin);
}

#[test]
fn pg18_xmin_reads_legacy_persistent_tuple_metadata_through_qualified_references() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-xmin.db");
    let expected_xmin = {
        let eng = Engine::open(&path).unwrap();
        eng.sql("CREATE TABLE versioned (a INTEGER)", &[]).unwrap();
        eng.sql("INSERT INTO versioned VALUES (1)", &[]).unwrap();
        integer_column(&eng.sql("SELECT xmin FROM versioned", &[]).unwrap(), "xmin")[0]
    };

    let connection = rusqlite::Connection::open(&path).unwrap();
    let body: String = connection
        .query_row("SELECT body FROM _documents WHERE doc_id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut legacy: Document = serde_json::from_str(&body).unwrap();
    assert_eq!(
        legacy.remove("\0uqa.system.xmin"),
        Some(uqa_core::Value::Int(expected_xmin))
    );
    connection
        .execute(
            "UPDATE _documents SET body = ?1 WHERE doc_id = 1",
            [serde_json::to_string(&legacy).unwrap()],
        )
        .unwrap();
    drop(connection);

    let eng = Engine::open(&path).unwrap();
    assert_eq!(
        integer_column(&eng.sql("SELECT xmin FROM versioned", &[]).unwrap(), "xmin",),
        [expected_xmin]
    );
    assert_eq!(
        integer_column(
            &eng.sql("SELECT v.xmin FROM versioned AS v", &[]).unwrap(),
            "xmin",
        ),
        [expected_xmin]
    );
}

#[test]
fn legacy_user_xmin_values_are_not_overwritten_by_tuple_version_metadata() {
    let eng = Engine::new();
    eng.create_default_table("legacy_xmin", Vec::new()).unwrap();
    eng.register_column(
        "legacy_xmin",
        uqa_sql::ast::ColumnDef {
            name: "xmin".into(),
            ty: uqa_sql::ast::ColumnType::Text,
            object_id: None,
            missing_value: None,
            primary_key: false,
            not_null: false,
            not_null_explicit: false,
            not_null_name: None,
            not_null_validated: true,
            not_null_no_inherit: false,
            auto_increment: None,
            unique: false,
            default: None,
            generated: None,
            check: None,
            check_name: None,
            check_enforced: true,
            check_validated: true,
            check_no_inherit: false,
            references: None,
        },
    )
    .unwrap();
    eng.add_document(
        "legacy_xmin",
        1,
        Document::from([("xmin".into(), uqa_core::Value::Str("user value".into()))]),
    )
    .unwrap();
    let document = eng.get_document("legacy_xmin", 1).unwrap().unwrap();
    assert_eq!(
        document.get("xmin"),
        Some(&uqa_core::Value::Str("user value".into()))
    );
    assert_eq!(
        document.len(),
        1,
        "internal tuple metadata leaked: {document:?}"
    );
}

#[test]
fn schemaless_system_xmin_mirror_is_refreshed_without_becoming_user_data() {
    let eng = Engine::new();
    eng.create_default_table("schemaless_xmin", Vec::new())
        .unwrap();
    eng.add_document(
        "schemaless_xmin",
        1,
        Document::from([("value".into(), uqa_core::Value::Int(10))]),
    )
    .unwrap();
    let first = eng.get_document("schemaless_xmin", 1).unwrap().unwrap()["xmin"].clone();
    let updated = eng
        .sql("UPDATE schemaless_xmin SET value = 20 RETURNING xmin", &[])
        .unwrap();
    assert_ne!(updated.rows[0]["xmin"], first);
    let document = eng.get_document("schemaless_xmin", 1).unwrap().unwrap();
    assert_ne!(document["xmin"], first);
    let uqa_core::Value::Int(expected_xmin) = document["xmin"] else {
        panic!("system xmin is not an integer: {document:?}");
    };
    assert_eq!(
        integer_column(
            &eng.sql("SELECT xmin FROM schemaless_xmin", &[]).unwrap(),
            "xmin"
        ),
        [expected_xmin]
    );
}

#[test]
fn schemaless_user_xmin_equal_to_tuple_version_is_not_misclassified_as_a_mirror() {
    let eng = Engine::new();
    eng.create_default_table("schemaless_user_xmin", Vec::new())
        .unwrap();
    eng.add_document(
        "schemaless_user_xmin",
        1,
        Document::from([("value".into(), uqa_core::Value::Int(10))]),
    )
    .unwrap();
    let first_system_xmin = integer_column(
        &eng.sql(
            "SELECT xmin FROM schemaless_user_xmin WHERE value = 10",
            &[],
        )
        .unwrap(),
        "xmin",
    )[0];
    let colliding_user_xmin = first_system_xmin + 1;
    eng.add_document(
        "schemaless_user_xmin",
        2,
        Document::from([
            ("xmin".into(), uqa_core::Value::Int(colliding_user_xmin)),
            ("value".into(), uqa_core::Value::Int(20)),
        ]),
    )
    .unwrap();
    assert_eq!(
        integer_column(
            &eng.sql(
                "SELECT xmin FROM schemaless_user_xmin WHERE value = 20",
                &[],
            )
            .unwrap(),
            "xmin",
        ),
        [colliding_user_xmin]
    );
    eng.update_document_fields(
        "schemaless_user_xmin",
        2,
        std::collections::BTreeMap::from([("value".into(), uqa_core::Value::Int(30))]),
        std::collections::BTreeMap::new(),
    )
    .unwrap();
    let document = eng
        .get_document("schemaless_user_xmin", 2)
        .unwrap()
        .unwrap();
    assert_eq!(
        document,
        Document::from([
            ("value".into(), uqa_core::Value::Int(30)),
            ("xmin".into(), uqa_core::Value::Int(colliding_user_xmin)),
        ])
    );
}
