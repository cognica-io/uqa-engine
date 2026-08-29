//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `ANALYZE` statistics persistence coverage.
//!
//! The histogram and MCV payloads must survive an `Engine::open` round trip
//! so the planner keeps its selectivity inputs after restart.

use std::sync::Arc;

use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage::{
    sqlite::{Catalog, ManagedConnection},
    ColumnStatsInput, SQLiteStorageBackend,
};

fn exec(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap();
}

fn insert_skewed_rows(engine: &Engine) {
    for i in 1..=100 {
        let cat = if i <= 60 { "A" } else { "B" };
        exec(
            engine,
            &format!("INSERT INTO t (id, val, cat) VALUES ({i}, {i}, '{cat}')"),
        );
    }
}

fn write_persisted_row_count(db_path: &std::path::Path, row_count: i64) {
    let conn = ManagedConnection::open(db_path).unwrap();
    let catalog = Catalog::open(conn).unwrap();
    catalog
        .save_column_stats(ColumnStatsInput::basic(
            "public.t", "val", 1, 0, None, None, row_count,
        ))
        .unwrap();
}

#[test]
fn analyze_histogram_and_mcv_survive_engine_reopen() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("uqa.sqlite3");

    let original = {
        let engine = Engine::open(&db_path).unwrap();
        exec(
            &engine,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER, cat TEXT)",
        );
        insert_skewed_rows(&engine);
        exec(&engine, "ANALYZE t");

        let stats = engine.column_stats("t").unwrap();
        let val = stats.get("val").expect("val stats");
        assert!(val.histogram.len() >= 2);
        assert_eq!(val.histogram.first(), Some(&Value::Int(1)));
        assert_eq!(val.histogram.last(), Some(&Value::Int(100)));

        let cat = stats.get("cat").expect("cat stats");
        assert!(cat.mcv_values.contains(&Value::Str("A".into())));
        let pos = cat
            .mcv_values
            .iter()
            .position(|v| v == &Value::Str("A".into()))
            .unwrap();
        assert!((cat.mcv_frequencies[pos] - 0.6).abs() < 1e-12);
        stats
    };

    let reopened = Engine::open(&db_path).unwrap();
    let restored = reopened.column_stats("t").unwrap();
    assert_eq!(restored["val"].histogram, original["val"].histogram);
    assert_eq!(restored["cat"].mcv_values, original["cat"].mcv_values);
    assert_eq!(
        restored["cat"].mcv_frequencies,
        original["cat"].mcv_frequencies
    );
}

#[test]
fn persisted_column_stats_refresh_after_an_external_commit() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("uqa.sqlite3");

    {
        let engine = Engine::open(&db_path).unwrap();
        exec(
            &engine,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)",
        );
        exec(&engine, "INSERT INTO t (id, val) VALUES (1, 10)");
    }

    write_persisted_row_count(&db_path, 999);
    // Exercise the storage-neutral open boundary directly. Change monitoring
    // is a backend capability, so this low-level constructor must refresh just
    // like `Engine::open` when another writer commits.
    let connection = ManagedConnection::open(&db_path).unwrap();
    let catalog = Arc::new(Catalog::open(connection.clone()).unwrap());
    let backend = Arc::new(SQLiteStorageBackend::new(connection));
    let reopened = Engine::from_persistent_backends(catalog, backend).unwrap();
    assert_eq!(reopened.column_stats("t").unwrap()["val"].row_count, 999);

    write_persisted_row_count(&db_path, 123);
    assert_eq!(reopened.column_stats("t").unwrap()["val"].row_count, 123);
}

#[test]
fn analyze_persists_stats_only_under_the_canonical_relation_name() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("uqa.sqlite3");

    {
        let engine = Engine::open(&db_path).unwrap();
        exec(&engine, "CREATE SCHEMA app");
        exec(&engine, "SET search_path TO app");
        exec(
            &engine,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)",
        );
        exec(&engine, "INSERT INTO t (id, val) VALUES (1, 10), (2, 20)");
        exec(&engine, "ANALYZE t");
    }

    let conn = ManagedConnection::open(&db_path).unwrap();
    let catalog = Catalog::open(conn).unwrap();
    assert!(catalog.load_column_stats("t").unwrap().is_empty());
    assert!(catalog.load_column_stats("public.t").unwrap().is_empty());
    let stats = catalog.load_column_stats("app.t").unwrap();
    assert_eq!(stats.len(), 2);
    assert_eq!(stats[0].row_count, 2);
    assert_eq!(stats[1].row_count, 2);
}

#[test]
fn analyze_without_table_name_persists_every_table() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("uqa.sqlite3");

    {
        let engine = Engine::open(&db_path).unwrap();
        exec(
            &engine,
            "CREATE TABLE a (id INTEGER PRIMARY KEY, x INTEGER)",
        );
        exec(&engine, "CREATE TABLE b (id INTEGER PRIMARY KEY, y TEXT)");
        exec(&engine, "INSERT INTO a (id, x) VALUES (1, 10), (2, 20)");
        exec(
            &engine,
            "INSERT INTO b (id, y) VALUES (1, 'left'), (2, 'right')",
        );
        exec(&engine, "ANALYZE");
    }

    let reopened = Engine::open(&db_path).unwrap();
    assert_eq!(reopened.column_stats("a").unwrap()["x"].row_count, 2);
    assert_eq!(reopened.column_stats("b").unwrap()["y"].distinct_count, 2);
}

#[test]
fn analyze_named_missing_table_is_an_error() {
    let engine = Engine::new();

    let direct = engine
        .run_analyze(Some("missing"))
        .expect_err("direct ANALYZE must not silently ignore a missing target");
    assert!(direct.to_string().contains("does not exist"));

    let sql = engine
        .sql("ANALYZE missing", &[])
        .expect_err("SQL ANALYZE must not report success for a missing target");
    assert!(sql.to_string().contains("does not exist"));
}

#[test]
fn column_stats_refresh_lazily_after_dml_without_manual_analyze() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER, cat TEXT)",
    );
    exec(
        &engine,
        "INSERT INTO t (id, val, cat) VALUES (1, 10, 'A'), (2, 20, 'B')",
    );

    let stats = engine.column_stats("t").unwrap();
    assert_eq!(stats["val"].row_count, 2);
    assert_eq!(stats["cat"].distinct_count, 2);

    exec(&engine, "INSERT INTO t (id, val, cat) VALUES (3, 30, 'B')");
    let stats = engine.column_stats("t").unwrap();
    assert_eq!(stats["val"].row_count, 3);
    assert_eq!(
        stats["cat"].mcv_values.first(),
        Some(&Value::Str("B".into()))
    );

    exec(&engine, "UPDATE t SET cat = 'A' WHERE id = 3");
    let stats = engine.column_stats("t").unwrap();
    assert_eq!(stats["cat"].distinct_count, 2);
    assert!(stats["cat"].mcv_values.contains(&Value::Str("A".into())));

    exec(&engine, "DELETE FROM t WHERE id = 2");
    let stats = engine.column_stats("t").unwrap();
    assert_eq!(stats["val"].row_count, 2);
    assert_eq!(stats["cat"].distinct_count, 1);
}

#[test]
fn dirty_column_stats_do_not_survive_reopen_as_stale_catalog_rows() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("uqa.sqlite3");

    {
        let engine = Engine::open(&db_path).unwrap();
        exec(
            &engine,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)",
        );
        exec(&engine, "INSERT INTO t (id, val) VALUES (1, 10), (2, 20)");
        assert_eq!(engine.column_stats("t").unwrap()["val"].row_count, 2);

        exec(&engine, "INSERT INTO t (id, val) VALUES (3, 30)");
    }

    let reopened = Engine::open(&db_path).unwrap();
    let stats = reopened.column_stats("t").unwrap();
    assert_eq!(stats["val"].row_count, 3);
    assert_eq!(stats["val"].max_value, Some(Value::Int(30)));
}

#[test]
fn drop_table_removes_persisted_column_stats() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("uqa.sqlite3");

    {
        let engine = Engine::open(&db_path).unwrap();
        exec(
            &engine,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER, cat TEXT)",
        );
        insert_skewed_rows(&engine);
        exec(&engine, "ANALYZE t");
        assert!(!engine.column_stats("t").unwrap().is_empty());
        exec(&engine, "DROP TABLE t");
    }

    let conn = ManagedConnection::open(&db_path).unwrap();
    let catalog = Catalog::open(conn).unwrap();
    assert!(catalog.load_column_stats("public.t").unwrap().is_empty());
}

#[test]
fn analyzed_stats_are_invalidated_after_dml_and_same_name_recreation() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("analyze-invalidation.sqlite3");
    let engine = Engine::open(&db_path).unwrap();
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)",
    );
    exec(&engine, "INSERT INTO t VALUES (1, 10), (2, 20)");
    exec(&engine, "ANALYZE t");
    let observer = engine.new_session().unwrap();
    assert_eq!(observer.column_stats("t").unwrap()["val"].row_count, 2);

    exec(&engine, "INSERT INTO t VALUES (3, 30)");
    assert_eq!(observer.column_stats("t").unwrap()["val"].row_count, 3);

    exec(&engine, "DROP TABLE t");
    exec(
        &engine,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)",
    );
    exec(&engine, "INSERT INTO t VALUES (1, 99)");
    assert_eq!(observer.column_stats("t").unwrap()["val"].row_count, 1);
}

#[test]
fn truncate_invalidates_stats_under_the_resolved_schema_name() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("uqa.sqlite3");

    {
        let engine = Engine::open(&db_path).unwrap();
        exec(&engine, "CREATE SCHEMA app");
        exec(&engine, "SET search_path TO app");
        exec(
            &engine,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)",
        );
        exec(&engine, "INSERT INTO t VALUES (1, 10), (2, 20)");
        exec(&engine, "ANALYZE t");
        assert_eq!(engine.column_stats("t").unwrap()["val"].row_count, 2);
        exec(&engine, "TRUNCATE t");
    }

    let reopened = Engine::open(&db_path).unwrap();
    exec(&reopened, "SET search_path TO app");
    assert_eq!(reopened.column_stats("t").unwrap()["val"].row_count, 0);
}

#[test]
fn analyze_after_a_transactional_write_is_immediately_visible_and_survives_rollback() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("analyze-after-write.sqlite3");
    {
        let engine = Engine::open(&db_path).unwrap();
        exec(
            &engine,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)",
        );
        exec(&engine, "INSERT INTO t VALUES (1, 10)");
        exec(&engine, "ANALYZE t");
        let observer = engine.new_session().unwrap();

        exec(&engine, "BEGIN");
        exec(&engine, "INSERT INTO t VALUES (2, 20)");
        exec(&engine, "SET TRANSACTION READ ONLY");
        exec(&engine, "ANALYZE t");
        assert_eq!(observer.column_stats("t").unwrap()["val"].row_count, 2);
        exec(&engine, "ROLLBACK");
        let count = engine.sql("SELECT count(*) AS n FROM t", &[]).unwrap();
        assert_eq!(count.rows[0]["n"], Value::Int(1));
    }

    let reopened = Engine::open(&db_path).unwrap();
    assert_eq!(reopened.column_stats("t").unwrap()["val"].row_count, 2);
}

#[test]
fn compressed_read_only_analyze_and_analyze_after_write_do_not_self_block() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("read-only-analyze.uqac.sqlite3");
    {
        let engine =
            Engine::open_compressed(&db_path, uqa_storage::SQLiteCompressionOptions::default())
                .unwrap();
        exec(
            &engine,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)",
        );
        exec(&engine, "INSERT INTO t VALUES (1, 10)");

        exec(&engine, "BEGIN READ ONLY");
        exec(&engine, "ANALYZE t");
        exec(&engine, "ROLLBACK");

        exec(&engine, "BEGIN");
        exec(&engine, "INSERT INTO t VALUES (2, 20)");
        exec(&engine, "SET TRANSACTION READ ONLY");
        exec(&engine, "ANALYZE t");
        exec(&engine, "ROLLBACK");
    }

    let reopened =
        Engine::open_compressed(&db_path, uqa_storage::SQLiteCompressionOptions::default())
            .unwrap();
    assert_eq!(reopened.column_stats("t").unwrap()["val"].row_count, 2);
}

#[test]
fn analyze_statistics_survive_savepoint_rollback() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("analyze-savepoint.sqlite3");
    {
        let engine = Engine::open(&db_path).unwrap();
        exec(
            &engine,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)",
        );
        exec(&engine, "INSERT INTO t VALUES (1, 10)");
        exec(&engine, "BEGIN");
        exec(&engine, "SAVEPOINT before_rows");
        exec(&engine, "INSERT INTO t VALUES (2, 20), (3, 30)");
        exec(&engine, "ANALYZE t");
        exec(&engine, "ROLLBACK TO SAVEPOINT before_rows");
        exec(&engine, "COMMIT");
        let count = engine.sql("SELECT count(*) AS n FROM t", &[]).unwrap();
        assert_eq!(count.rows[0]["n"], Value::Int(1));
    }

    let reopened = Engine::open(&db_path).unwrap();
    assert_eq!(reopened.column_stats("t").unwrap()["val"].row_count, 3);
}

#[test]
fn analyze_of_rolled_back_relation_does_not_attach_to_same_name_recreation() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("analyze-rolled-back-create.sqlite3");
    {
        let engine = Engine::open(&db_path).unwrap();
        exec(&engine, "BEGIN");
        exec(&engine, "CREATE TABLE t (id INTEGER)");
        exec(&engine, "INSERT INTO t VALUES (1), (2)");
        exec(&engine, "ANALYZE t");
        exec(&engine, "ROLLBACK");
        exec(&engine, "CREATE TABLE t (id INTEGER)");
        assert_eq!(engine.column_stats("t").unwrap()["id"].row_count, 0);
    }

    let reopened = Engine::open(&db_path).unwrap();
    assert_eq!(reopened.column_stats("t").unwrap()["id"].row_count, 0);
}

#[test]
fn analyze_of_same_name_replacement_does_not_overwrite_rolled_back_relation_stats() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("analyze-rolled-back-replacement.sqlite3");
    {
        let engine = Engine::open(&db_path).unwrap();
        exec(&engine, "CREATE TABLE t (id INTEGER)");
        exec(&engine, "INSERT INTO t VALUES (1)");
        exec(&engine, "ANALYZE t");
        exec(&engine, "BEGIN");
        exec(&engine, "DROP TABLE t");
        exec(&engine, "CREATE TABLE t (id INTEGER)");
        exec(&engine, "INSERT INTO t VALUES (2), (3)");
        exec(&engine, "ANALYZE t");
        exec(&engine, "ROLLBACK");
        assert_eq!(engine.column_stats("t").unwrap()["id"].row_count, 1);
    }

    let reopened = Engine::open(&db_path).unwrap();
    assert_eq!(reopened.column_stats("t").unwrap()["id"].row_count, 1);
}

#[test]
fn analyze_after_rolled_back_rename_does_not_rebind_stats_by_table_state_alone() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("analyze-rolled-back-rename.sqlite3");
    {
        let engine = Engine::open(&db_path).unwrap();
        exec(&engine, "CREATE TABLE t (id INTEGER)");
        exec(&engine, "INSERT INTO t VALUES (1)");
        exec(&engine, "BEGIN");
        exec(&engine, "INSERT INTO t VALUES (2)");
        exec(&engine, "ALTER TABLE t RENAME TO renamed_t");
        exec(&engine, "ANALYZE renamed_t");
        exec(&engine, "ROLLBACK");
        assert_eq!(engine.column_stats("t").unwrap()["id"].row_count, 1);
    }

    let reopened = Engine::open(&db_path).unwrap();
    assert_eq!(reopened.column_stats("t").unwrap()["id"].row_count, 1);
}

#[test]
fn analyze_savepoint_restore_tracks_relation_lifetimes_at_each_boundary() {
    let dir = tempdir().unwrap();
    let db_path = dir
        .path()
        .join("analyze-savepoint-relation-lifetime.sqlite3");
    {
        let engine = Engine::open(&db_path).unwrap();
        exec(&engine, "BEGIN");
        exec(&engine, "CREATE TABLE survivor (id INTEGER)");
        exec(&engine, "INSERT INTO survivor VALUES (1)");
        exec(&engine, "SAVEPOINT before_transient");
        exec(&engine, "ANALYZE survivor");
        exec(&engine, "CREATE TABLE transient (id INTEGER)");
        exec(&engine, "INSERT INTO transient VALUES (1), (2)");
        exec(&engine, "ANALYZE transient");
        exec(&engine, "ROLLBACK TO SAVEPOINT before_transient");
        assert_eq!(engine.column_stats("survivor").unwrap()["id"].row_count, 1);
        assert!(engine.sql("SELECT * FROM transient", &[]).is_err());
        exec(&engine, "ROLLBACK");
        exec(&engine, "CREATE TABLE survivor (id INTEGER)");
        exec(&engine, "CREATE TABLE transient (id INTEGER)");
        assert_eq!(engine.column_stats("survivor").unwrap()["id"].row_count, 0);
        assert_eq!(engine.column_stats("transient").unwrap()["id"].row_count, 0);
    }

    let reopened = Engine::open(&db_path).unwrap();
    assert_eq!(
        reopened.column_stats("survivor").unwrap()["id"].row_count,
        0
    );
    assert_eq!(
        reopened.column_stats("transient").unwrap()["id"].row_count,
        0
    );
}
