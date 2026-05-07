//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of the ANALYZE persistence coverage from
//! `uqa/tests/test_cost_optimizer.py` and `test_catalog.py`.
//!
//! The important parity point is not just that ANALYZE runs: the
//! histogram and MCV payloads must survive an `Engine::open` round trip
//! so the planner keeps its selectivity inputs after restart.

use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage::sqlite::{Catalog, ManagedConnection};

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

        let stats = engine.column_stats("t");
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
    let restored = reopened.column_stats("t");
    assert_eq!(restored["val"].histogram, original["val"].histogram);
    assert_eq!(restored["cat"].mcv_values, original["cat"].mcv_values);
    assert_eq!(
        restored["cat"].mcv_frequencies,
        original["cat"].mcv_frequencies
    );
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
    assert_eq!(reopened.column_stats("a")["x"].row_count, 2);
    assert_eq!(reopened.column_stats("b")["y"].distinct_count, 2);
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
        assert!(!engine.column_stats("t").is_empty());
        exec(&engine, "DROP TABLE t");
    }

    let conn = ManagedConnection::open(&db_path).unwrap();
    let catalog = Catalog::open(conn).unwrap();
    assert!(catalog.load_column_stats("t").unwrap().is_empty());
}
