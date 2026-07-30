//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for the ANALYZE persistence coverage from
//! `test_cost_optimizer` and `test_catalog`.
//!
//! The important parity point is not just that ANALYZE runs: the
//! histogram and MCV payloads must survive an `Engine::open` round trip
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
fn persisted_column_stats_are_loaded_during_open() {
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
    // Exercise the storage-neutral open boundary directly. `Engine::open`
    // attaches an external-commit monitor after this restore; that monitor is
    // intentionally allowed to replace the cache when the write below
    // commits, which would mask whether the restore itself was eager.
    let connection = ManagedConnection::open(&db_path).unwrap();
    let catalog = Arc::new(Catalog::open(connection.clone()).unwrap());
    let backend = Arc::new(SQLiteStorageBackend::new(connection));
    let reopened = Engine::from_persistent_backends(catalog, backend).unwrap();
    write_persisted_row_count(&db_path, 123);

    let stats = reopened.column_stats("t").unwrap();
    assert_eq!(stats["val"].row_count, 999);
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
