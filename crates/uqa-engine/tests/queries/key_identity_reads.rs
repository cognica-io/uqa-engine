//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A filter that compares a table's single integer primary key with a value that reads no row reads only the row at the identity that value names, from storage or from the rows the running statement staged, and still evaluates the whole filter. A key value that names no identity, such as a negative key, reads the table instead.

use uqa_core::Value;
use uqa_engine::{Engine, SQLParam};

fn run(engine: &Engine, statement: &str) {
    engine
        .sql(statement, &[])
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

fn integers(engine: &Engine, query: &str, params: &[SQLParam]) -> Vec<i64> {
    engine
        .sql(query, params)
        .unwrap_or_else(|error| panic!("{query}: {error}"))
        .rows
        .iter()
        .map(|row| match row.values().next() {
            Some(Value::Int(value)) => *value,
            other => panic!("{query}: unexpected value {other:?}"),
        })
        .collect()
}

fn engines(directory: &std::path::Path) -> Vec<(&'static str, Engine)> {
    vec![
        ("memory", Engine::new()),
        (
            "native SQLite",
            Engine::open(&directory.join("key-identity-reads.db")).unwrap(),
        ),
    ]
}

#[test]
fn a_key_compared_with_a_value_that_reads_no_row_selects_its_row() {
    let directory = tempfile::tempdir().unwrap();
    for (label, engine) in engines(directory.path()) {
        run(
            &engine,
            "CREATE TABLE keyed (id bigint PRIMARY KEY, v integer);
             INSERT INTO keyed VALUES (1, 10), (2, 20), (3, 30), (-1, 40), (4611686018427387904, 50)",
        );
        let ten = [SQLParam::scalar(Value::Int(3))];
        for (query, params, expected) in [
            ("SELECT v FROM keyed WHERE id = $1 - 1", &ten[..], vec![20]),
            ("SELECT v FROM keyed WHERE id = 1 + 2", &[][..], vec![30]),
            ("SELECT v FROM keyed WHERE 2 = id", &[][..], vec![20]),
            (
                "SELECT v FROM keyed WHERE id IN (3, 1, 99) ORDER BY v",
                &[][..],
                vec![10, 30],
            ),
            (
                "SELECT v FROM keyed WHERE id = 2 AND v = 99",
                &[][..],
                vec![],
            ),
            ("SELECT v FROM keyed WHERE id = 99", &[][..], vec![]),
            // Keys that name no identity belong to rows at other identities.
            ("SELECT v FROM keyed WHERE id = -1", &[][..], vec![40]),
            (
                "SELECT v FROM keyed WHERE id = 4611686018427387904",
                &[][..],
                vec![50],
            ),
            (
                "SELECT v FROM keyed WHERE id IN (-1, 2) ORDER BY v",
                &[][..],
                vec![20, 40],
            ),
            // An alias and a join read through the general scan.
            (
                "SELECT k.v FROM keyed AS k WHERE k.id = $1 - 2",
                &ten[..],
                vec![10],
            ),
            (
                "SELECT k.value FROM keyed k(key_column, value) WHERE key_column = 3",
                &[][..],
                vec![30],
            ),
            (
                "SELECT b.v FROM keyed a JOIN keyed b ON b.id = a.id + 1 WHERE a.id = 2",
                &[][..],
                vec![30],
            ),
            (
                "SELECT count(*) FROM keyed AS k WHERE k.id = 3",
                &[][..],
                vec![1],
            ),
            (
                "SELECT count(*) FROM keyed AS k WHERE k.id = 7",
                &[][..],
                vec![0],
            ),
        ] {
            assert_eq!(
                integers(&engine, query, params),
                expected,
                "{label}: {query}"
            );
        }
    }
}

#[test]
fn a_volatile_function_reads_by_key_the_rows_its_statement_staged() {
    let directory = tempfile::tempdir().unwrap();
    for (label, engine) in engines(directory.path()) {
        run(
            &engine,
            "CREATE TABLE chain (id integer PRIMARY KEY, v integer NOT NULL);
             CREATE FUNCTION previous(k integer) RETURNS integer VOLATILE LANGUAGE sql AS $$ SELECT c.v FROM chain AS c WHERE c.id = k - 1 $$;
             CREATE FUNCTION present(k integer) RETURNS bigint VOLATILE LANGUAGE sql AS $$ SELECT count(*) FROM chain WHERE id = k $$",
        );
        run(
            &engine,
            "INSERT INTO chain SELECT g, coalesce(previous(g), 0) + 1 FROM generate_series(1, 5) AS g",
        );
        assert_eq!(
            integers(&engine, "SELECT v FROM chain ORDER BY id", &[]),
            [1, 2, 3, 4, 5],
            "{label}"
        );
        // Each row reads its predecessor as this statement left it.
        run(
            &engine,
            "UPDATE chain SET v = coalesce(previous(id), 0) + 10 * v",
        );
        assert_eq!(
            integers(&engine, "SELECT v FROM chain ORDER BY id", &[]),
            [10, 30, 60, 100, 150],
            "{label}"
        );
        // A row the statement deleted is gone for the rows after it.
        assert_eq!(
            integers(
                &engine,
                "DELETE FROM chain WHERE id IN (2, 3) OR (id = 4 AND present(3) = 0) RETURNING id",
                &[],
            ),
            [2, 3, 4],
            "{label}"
        );
    }
}

#[test]
fn a_keyed_read_in_a_repeatable_read_transaction_sees_its_own_rows_and_not_later_commits() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("repeatable-keys.db")).unwrap();
    run(
        &engine,
        "CREATE TABLE keyed (id integer PRIMARY KEY, v integer);
         INSERT INTO keyed VALUES (1, 10)",
    );
    let other = engine.new_session().unwrap();
    run(&engine, "BEGIN ISOLATION LEVEL REPEATABLE READ");
    assert_eq!(
        integers(&engine, "SELECT v FROM keyed WHERE id = 1", &[]),
        [10]
    );
    run(&other, "INSERT INTO keyed VALUES (2, 20)");
    run(&engine, "INSERT INTO keyed VALUES (3, 30)");
    run(&engine, "UPDATE keyed SET v = 11 WHERE id = 1");
    for (query, expected) in [
        ("SELECT k.v FROM keyed AS k WHERE k.id = 1", vec![11]),
        ("SELECT k.v FROM keyed AS k WHERE k.id = 2", vec![]),
        ("SELECT k.v FROM keyed AS k WHERE k.id = 3", vec![30]),
        ("SELECT v FROM keyed WHERE id = 1 + 2", vec![30]),
    ] {
        assert_eq!(integers(&engine, query, &[]), expected, "{query}");
    }
    run(&engine, "COMMIT");
    assert_eq!(
        integers(&engine, "SELECT v FROM keyed AS k WHERE k.id = 2", &[]),
        [20]
    );
}

#[test]
fn serializable_transactions_that_each_read_the_key_the_other_inserts_cannot_both_commit() {
    let directory = tempfile::tempdir().unwrap();
    let first = Engine::open(&directory.path().join("serializable-keys.db")).unwrap();
    run(
        &first,
        "CREATE TABLE keyed (id integer PRIMARY KEY, v integer)",
    );
    let second = first.new_session().unwrap();
    run(&first, "BEGIN ISOLATION LEVEL SERIALIZABLE");
    run(&second, "BEGIN ISOLATION LEVEL SERIALIZABLE");
    assert_eq!(
        integers(
            &first,
            "SELECT count(*) FROM keyed AS k WHERE k.id = 1 + 1",
            &[]
        ),
        [0]
    );
    assert_eq!(
        integers(
            &second,
            "SELECT count(*) FROM keyed AS k WHERE k.id = 0 + 1",
            &[]
        ),
        [0]
    );
    run(&first, "INSERT INTO keyed VALUES (1, 1)");
    let inserted = second.sql("INSERT INTO keyed VALUES (2, 2)", &[]);
    let committed = first.sql("COMMIT", &[]);
    let later = inserted.and_then(|_| second.sql("COMMIT", &[]));
    let failures = [committed.err(), later.err()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].sqlstate(), Some("40001"), "{}", failures[0]);
}
