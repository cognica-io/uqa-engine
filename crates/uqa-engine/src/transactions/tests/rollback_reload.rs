//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A rollback reloads the catalog at one snapshot and records it, so the next statement refreshes only what other sessions committed since; reads and the session's own data commits leave the table states alone on every provider, whether or not its catalog reports cache revisions.

use std::sync::Arc;

use super::mutation_failures::persistent_engine;
use crate::Value;

fn value(engine: &super::Engine, table: &str) -> Value {
    engine
        .sql(&format!("SELECT v FROM {table} WHERE id = 1"), &[])
        .unwrap()
        .rows[0]["v"]
        .clone()
}

#[test]
fn the_statement_after_a_rollback_keeps_the_tables_the_rollback_reloaded() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let engine = persistent_engine(provider, &directory.path().join("rollback-reload.db"));
        engine
            .sql(
                "CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER); INSERT INTO items VALUES (1, 1)",
                &[],
            )
            .unwrap();
        for rollback in [
            "BEGIN; UPDATE items SET v = 2 WHERE id = 1; ROLLBACK",
            "INSERT INTO items VALUES (2, 2), (1, 9)",
            "BEGIN; SAVEPOINT attempt; UPDATE items SET v = 3 WHERE id = 1; ROLLBACK TO attempt",
        ] {
            let _ = engine.sql(rollback, &[]);
            let reloaded = engine.try_table("items").unwrap().unwrap();
            assert_eq!(
                value(&engine, "items"),
                Value::Int(1),
                "{provider}: {rollback}"
            );
            let after = engine.try_table("items").unwrap().unwrap();
            assert!(
                Arc::ptr_eq(&reloaded, &after),
                "{provider}: the statement after `{rollback}` reloaded the catalog again"
            );
        }
        engine.sql("COMMIT", &[]).unwrap();
    }
}

#[test]
fn commits_of_another_session_after_a_rollback_reach_the_next_statement() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let engine = persistent_engine(provider, &directory.path().join("rollback-commits.db"));
        engine
            .sql(
                "CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER); INSERT INTO items VALUES (1, 1)",
                &[],
            )
            .unwrap();
        let other = engine.new_session().unwrap();
        engine
            .sql("BEGIN; UPDATE items SET v = 2 WHERE id = 1; ROLLBACK", &[])
            .unwrap();
        other
            .sql(
                "UPDATE items SET v = 5 WHERE id = 1; CREATE TABLE added (id INTEGER PRIMARY KEY, v INTEGER); INSERT INTO added VALUES (1, 50)",
                &[],
            )
            .unwrap();
        assert_eq!(value(&engine, "items"), Value::Int(5), "{provider}");
        assert_eq!(value(&engine, "added"), Value::Int(50), "{provider}");
        // A savepoint rollback leaves a READ COMMITTED transaction open; its next statement sees the other session's later commit.
        engine
            .sql(
                "BEGIN; SAVEPOINT attempt; UPDATE items SET v = 6 WHERE id = 1; ROLLBACK TO attempt",
                &[],
            )
            .unwrap();
        other
            .sql("UPDATE items SET v = 7 WHERE id = 1; DROP TABLE added", &[])
            .unwrap();
        assert_eq!(value(&engine, "items"), Value::Int(7), "{provider}");
        assert_eq!(
            engine
                .sql("SELECT v FROM added", &[])
                .unwrap_err()
                .sqlstate(),
            Some("42P01"),
            "{provider}"
        );
        engine.sql("ROLLBACK", &[]).unwrap();
        // A rolled-back definition is gone, and a restored one is back.
        engine
            .sql(
                "BEGIN; CREATE TABLE fleeting (id INTEGER PRIMARY KEY); ALTER TABLE items RENAME COLUMN v TO w; ROLLBACK",
                &[],
            )
            .unwrap();
        assert_eq!(value(&engine, "items"), Value::Int(7), "{provider}");
        assert_eq!(
            engine
                .sql("SELECT * FROM fleeting", &[])
                .unwrap_err()
                .sqlstate(),
            Some("42P01"),
            "{provider}"
        );
    }
}

#[test]
fn reads_and_own_data_commits_keep_the_table_states_on_every_provider() {
    for provider in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let engine = persistent_engine(provider, &directory.path().join("statement-reload.db"));
        engine
            .sql(
                "CREATE TABLE items (id INTEGER PRIMARY KEY, v INTEGER); INSERT INTO items VALUES (1, 1); CREATE TABLE others (id INTEGER PRIMARY KEY, v INTEGER); INSERT INTO others VALUES (1, 10)",
                &[],
            )
            .unwrap();
        assert_eq!(value(&engine, "items"), Value::Int(1));
        let state = engine.try_table("others").unwrap().unwrap();
        for statement in [
            "SELECT v FROM items WHERE id = 1",
            "UPDATE items SET v = 2 WHERE id = 1",
            "SELECT v FROM items WHERE id = 1",
            "INSERT INTO items VALUES (2, 20)",
            "BEGIN; UPDATE items SET v = 3 WHERE id = 1; COMMIT",
        ] {
            engine.sql(statement, &[]).unwrap();
            assert_eq!(value(&engine, "others"), Value::Int(10));
            assert!(
                Arc::ptr_eq(&state, &engine.try_table("others").unwrap().unwrap()),
                "{provider}: `{statement}` reloaded the table states"
            );
        }
        assert_eq!(value(&engine, "items"), Value::Int(3), "{provider}");
        // Another session's commit still reaches the next statement.
        let other = engine.new_session().unwrap();
        other
            .sql("UPDATE others SET v = 11 WHERE id = 1", &[])
            .unwrap();
        assert_eq!(value(&engine, "others"), Value::Int(11), "{provider}");
    }
}
