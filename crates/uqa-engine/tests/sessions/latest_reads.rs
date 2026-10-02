//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A transaction reads the tables it has not written where their latest committed rows are stored, and sees its own writes, other sessions' commits and its isolation level's snapshot exactly as through the records.

use super::*;

fn fixture(path: &std::path::Path) -> Engine {
    let engine = Engine::open(path).unwrap();
    engine
        .sql(
            "CREATE TABLE items (id integer PRIMARY KEY, v integer NOT NULL); INSERT INTO items SELECT g, g FROM generate_series(1, 200) AS g; CREATE TABLE log (id integer PRIMARY KEY, note text)",
            &[],
        )
        .unwrap();
    engine
}

fn totals(engine: &Engine) -> (Value, Value) {
    let result = engine
        .sql("SELECT count(*) AS n, sum(v) AS total FROM items", &[])
        .unwrap();
    (result.rows[0]["n"].clone(), result.rows[0]["total"].clone())
}

fn first_rows(engine: &Engine) -> Vec<(Value, Value)> {
    engine
        .sql("SELECT id, v FROM items ORDER BY id LIMIT 3", &[])
        .unwrap()
        .rows
        .iter()
        .map(|row| (row["id"].clone(), row["v"].clone()))
        .collect()
}

#[test]
fn a_transaction_that_wrote_another_table_reads_its_own_writes_of_this_one() {
    let directory = tempfile::tempdir().unwrap();
    let engine = fixture(&directory.path().join("latest-own-writes.db"));
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("INSERT INTO log VALUES (1, 'written')", &[])
        .unwrap();
    assert_eq!(totals(&engine), (Value::Int(200), Value::Int(20_100)));
    assert_eq!(
        first_rows(&engine),
        [
            (Value::Int(1), Value::Int(1)),
            (Value::Int(2), Value::Int(2)),
            (Value::Int(3), Value::Int(3))
        ]
    );
    engine
        .sql("UPDATE items SET v = v + 100 WHERE id = 2", &[])
        .unwrap();
    engine.sql("DELETE FROM items WHERE id = 3", &[]).unwrap();
    assert_eq!(totals(&engine), (Value::Int(199), Value::Int(20_197)));
    assert_eq!(
        first_rows(&engine),
        [
            (Value::Int(1), Value::Int(1)),
            (Value::Int(2), Value::Int(102)),
            (Value::Int(4), Value::Int(4))
        ]
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(totals(&engine), (Value::Int(200), Value::Int(20_100)));
}

#[test]
fn a_truncation_in_the_transaction_rebinds_the_table_away_from_its_committed_rows() {
    let directory = tempfile::tempdir().unwrap();
    let engine = fixture(&directory.path().join("latest-truncation.db"));
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("INSERT INTO log VALUES (1, 'written')", &[])
        .unwrap();
    engine.sql("TRUNCATE items", &[]).unwrap();
    assert_eq!(totals(&engine), (Value::Int(0), Value::Null));
    assert!(first_rows(&engine).is_empty());
    engine.sql("INSERT INTO items VALUES (7, 70)", &[]).unwrap();
    assert_eq!(totals(&engine), (Value::Int(1), Value::Int(70)));
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(totals(&engine), (Value::Int(200), Value::Int(20_100)));
}

#[test]
fn each_isolation_level_keeps_its_snapshot_of_another_sessions_commit() {
    for (isolation, sees_commit) in [
        ("READ COMMITTED", true),
        ("REPEATABLE READ", false),
        ("SERIALIZABLE", false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let engine = fixture(&directory.path().join("latest-isolation.db"));
        let other = engine.new_session().unwrap();
        engine
            .sql(&format!("BEGIN ISOLATION LEVEL {isolation}"), &[])
            .unwrap();
        engine
            .sql("INSERT INTO log VALUES (1, 'written')", &[])
            .unwrap();
        assert_eq!(totals(&engine), (Value::Int(200), Value::Int(20_100)));
        other
            .sql("INSERT INTO items VALUES (201, 1000)", &[])
            .unwrap();
        let expected = if sees_commit {
            (Value::Int(201), Value::Int(21_100))
        } else {
            (Value::Int(200), Value::Int(20_100))
        };
        assert_eq!(totals(&engine), expected, "{isolation}");
        engine.sql("COMMIT", &[]).unwrap();
        assert_eq!(
            totals(&engine),
            (Value::Int(201), Value::Int(21_100)),
            "{isolation}"
        );
    }
}
