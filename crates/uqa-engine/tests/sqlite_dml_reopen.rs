//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT, UPDATE, DELETE round-trips through a `SQLite`-backed
//! engine plus an explicit drop/reopen cycle between every mutation.
//! Catches regressions in catalog persistence: any DML the in-memory
//! engine accepts must also survive a process restart on the
//! `SQLite` backend.

use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::Engine;

const SCHEMA: &str = "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT, qty INTEGER)";

/// Helper: open an engine on `path`, run `f`, then drop the engine.
/// Forces every mutation to round-trip through `SQLite` before the
/// next assertion runs.
fn with_engine<F, R>(path: &std::path::Path, f: F) -> R
where
    F: FnOnce(&Engine) -> R,
{
    let engine = Engine::open(path).expect("open engine");
    f(&engine)
}

fn select_all(engine: &Engine) -> Vec<(i64, String, i64)> {
    let r = engine
        .sql("SELECT id, label, qty FROM items ORDER BY id", &[])
        .expect("select");
    r.rows
        .iter()
        .filter_map(|row| {
            let id = match row.get("id") {
                Some(Value::Int(n)) => *n,
                _ => return None,
            };
            let label = match row.get("label") {
                Some(Value::Str(s)) => s.clone(),
                _ => return None,
            };
            let qty = match row.get("qty") {
                Some(Value::Int(n)) => *n,
                _ => return None,
            };
            Some((id, label, qty))
        })
        .collect()
}

#[test]
fn insert_survives_reopen() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");

    with_engine(&path, |eng| {
        eng.sql(SCHEMA, &[]).unwrap();
        eng.sql(
            "INSERT INTO items (id, label, qty) VALUES \
             (1, 'apple', 3), (2, 'banana', 7), (3, 'cherry', 2)",
            &[],
        )
        .unwrap();
    });

    let observed = with_engine(&path, select_all);
    assert_eq!(
        observed,
        vec![
            (1, "apple".into(), 3),
            (2, "banana".into(), 7),
            (3, "cherry".into(), 2),
        ],
    );
}

#[test]
fn update_survives_reopen() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");

    with_engine(&path, |eng| {
        eng.sql(SCHEMA, &[]).unwrap();
        eng.sql(
            "INSERT INTO items (id, label, qty) VALUES (1, 'apple', 3), (2, 'banana', 7)",
            &[],
        )
        .unwrap();
    });

    with_engine(&path, |eng| {
        let r = eng
            .sql("UPDATE items SET qty = 99 WHERE label = 'banana'", &[])
            .unwrap();
        assert_eq!(r.affected_rows, 1);
    });

    let observed = with_engine(&path, select_all);
    assert_eq!(
        observed,
        vec![(1, "apple".into(), 3), (2, "banana".into(), 99)],
    );
}

#[test]
fn delete_survives_reopen() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");

    with_engine(&path, |eng| {
        eng.sql(SCHEMA, &[]).unwrap();
        eng.sql(
            "INSERT INTO items (id, label, qty) VALUES \
             (1, 'apple', 3), (2, 'banana', 7), (3, 'cherry', 2)",
            &[],
        )
        .unwrap();
    });

    with_engine(&path, |eng| {
        let r = eng.sql("DELETE FROM items WHERE qty < 5", &[]).unwrap();
        assert_eq!(r.affected_rows, 2); // apple, cherry
    });

    let observed = with_engine(&path, select_all);
    assert_eq!(observed, vec![(2, "banana".into(), 7)]);
}

#[test]
fn mixed_dml_survives_multiple_reopens() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");

    // Cycle 1: create + initial insert.
    with_engine(&path, |eng| {
        eng.sql(SCHEMA, &[]).unwrap();
        eng.sql(
            "INSERT INTO items (id, label, qty) VALUES (1, 'a', 10)",
            &[],
        )
        .unwrap();
    });

    // Cycle 2: insert another row, update the first.
    with_engine(&path, |eng| {
        eng.sql(
            "INSERT INTO items (id, label, qty) VALUES (2, 'b', 20)",
            &[],
        )
        .unwrap();
        eng.sql("UPDATE items SET qty = qty + 5 WHERE id = 1", &[])
            .unwrap();
    });

    // Cycle 3: delete the second row.
    with_engine(&path, |eng| {
        eng.sql("DELETE FROM items WHERE id = 2", &[]).unwrap();
    });

    let observed = with_engine(&path, select_all);
    assert_eq!(observed, vec![(1, "a".into(), 15)]);
}

#[test]
fn update_arithmetic_survives_reopen() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");

    with_engine(&path, |eng| {
        eng.sql(SCHEMA, &[]).unwrap();
        eng.sql(
            "INSERT INTO items (id, label, qty) VALUES \
             (1, 'a', 1), (2, 'b', 2), (3, 'c', 3), (4, 'd', 4)",
            &[],
        )
        .unwrap();
        eng.sql("UPDATE items SET qty = qty * 10", &[]).unwrap();
    });

    let observed = with_engine(&path, select_all);
    assert_eq!(
        observed,
        vec![
            (1, "a".into(), 10),
            (2, "b".into(), 20),
            (3, "c".into(), 30),
            (4, "d".into(), 40),
        ],
    );
}

#[test]
fn delete_all_then_reinsert_survives_reopen() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("db.sqlite");

    with_engine(&path, |eng| {
        eng.sql(SCHEMA, &[]).unwrap();
        eng.sql("INSERT INTO items (id, label, qty) VALUES (1, 'x', 1)", &[])
            .unwrap();
        eng.sql("DELETE FROM items", &[]).unwrap();
        eng.sql("INSERT INTO items (id, label, qty) VALUES (2, 'y', 2)", &[])
            .unwrap();
    });

    let observed = with_engine(&path, select_all);
    assert_eq!(observed, vec![(2, "y".into(), 2)]);
}
