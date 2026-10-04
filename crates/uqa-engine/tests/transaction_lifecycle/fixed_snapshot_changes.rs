//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A transaction whose reads see a fixed snapshot sees its own changes above that snapshot: the rows it inserted, updated, deleted and moved, as savepoints, subtransactions and rollbacks left them, and never a row another session committed after the snapshot.

use uqa_core::Value;
use uqa_engine::Engine;

fn run(engine: &Engine, statement: &str) {
    engine
        .sql(statement, &[])
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

fn integers(engine: &Engine, query: &str) -> Vec<i64> {
    engine
        .sql(query, &[])
        .unwrap_or_else(|error| panic!("{query}: {error}"))
        .rows
        .iter()
        .map(|row| match row.values().next() {
            Some(Value::Int(value)) => *value,
            other => panic!("{query}: unexpected value {other:?}"),
        })
        .collect()
}

fn check(engine: &Engine, expectations: &[(&str, &[i64])]) {
    for (query, expected) in expectations {
        assert_eq!(integers(engine, query), *expected, "{query}");
    }
}

#[test]
fn a_repeatable_read_transaction_sees_its_changes_as_savepoints_leave_them() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("fixed-changes.db")).unwrap();
    run(
        &engine,
        "CREATE TABLE docs (id integer PRIMARY KEY, v integer NOT NULL);
         INSERT INTO docs SELECT g, g FROM generate_series(1, 10) AS g",
    );
    let other = engine.new_session().unwrap();
    for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
        run(&engine, &format!("BEGIN ISOLATION LEVEL {isolation}"));
        check(&engine, &[("SELECT count(*) FROM docs", &[10])]);
        run(&other, "INSERT INTO docs VALUES (100, 100)");
        run(&engine, "INSERT INTO docs VALUES (11, 11), (12, 12)");
        run(&engine, "UPDATE docs SET v = v * 10 WHERE id <= 2");
        run(&engine, "DELETE FROM docs WHERE id IN (3, 11)");
        // A key update moves the row to the identity its new key names.
        run(&engine, "UPDATE docs SET id = 40 WHERE id = 4");
        check(
            &engine,
            &[
                ("SELECT count(*) FROM docs", &[10]),
                (
                    "SELECT id FROM docs WHERE id IN (3, 4, 11, 40, 100) ORDER BY id",
                    &[40],
                ),
                ("SELECT v FROM docs WHERE id <= 2 ORDER BY id", &[10, 20]),
                (
                    "SELECT sum(v) FROM docs",
                    &[30 + 5 + 6 + 7 + 8 + 9 + 10 + 4 + 12],
                ),
            ],
        );
        run(&engine, "SAVEPOINT later");
        run(&engine, "DELETE FROM docs WHERE id > 5");
        run(&engine, "INSERT INTO docs VALUES (50, 50)");
        check(&engine, &[("SELECT count(*) FROM docs", &[4])]);
        run(&engine, "ROLLBACK TO SAVEPOINT later");
        check(
            &engine,
            &[
                ("SELECT count(*) FROM docs", &[10]),
                ("SELECT count(*) FROM docs WHERE id = 50", &[0]),
                ("SELECT count(*) FROM docs AS d WHERE d.id = 12", &[1]),
            ],
        );
        run(&engine, "RELEASE SAVEPOINT later");
        run(&engine, "UPDATE docs SET v = 0 WHERE id = 5");
        check(&engine, &[("SELECT v FROM docs WHERE id = 5", &[0])]);
        run(&engine, "ROLLBACK");
        run(&other, "DELETE FROM docs WHERE id = 100");
        check(&engine, &[("SELECT count(*) FROM docs", &[10])]);
    }
}

#[test]
fn subtransactions_that_fail_take_their_changes_with_them() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("fixed-subtransactions.db")).unwrap();
    run(
        &engine,
        "CREATE TABLE docs (id integer PRIMARY KEY, v text NOT NULL);
         CREATE FUNCTION fill(n integer) RETURNS bigint LANGUAGE plpgsql AS $$
         DECLARE total bigint := 0;
         BEGIN
           FOR i IN 1..n LOOP
             BEGIN
               INSERT INTO docs VALUES (i, 'x');
               IF i % 3 = 0 THEN RAISE EXCEPTION 'skip %', i; END IF;
             EXCEPTION WHEN raise_exception THEN NULL;
             END;
             total := total + (SELECT count(*) FROM docs WHERE id = i);
           END LOOP;
           RETURN total;
         END $$",
    );
    run(&engine, "BEGIN ISOLATION LEVEL REPEATABLE READ");
    check(&engine, &[("SELECT count(*) FROM docs", &[0])]);
    check(
        &engine,
        &[
            ("SELECT fill(30)", &[20]),
            ("SELECT count(*) FROM docs", &[20]),
            ("SELECT count(*) FROM docs WHERE id % 3 = 0", &[0]),
        ],
    );
    run(&engine, "COMMIT");
    check(&engine, &[("SELECT count(*) FROM docs", &[20])]);
}
