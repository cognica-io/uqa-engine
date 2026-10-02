//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::atomic::Ordering;

use super::DocumentCountChange;
use crate::Engine;

/// Whether the cached count is clean, and the cached count.
fn cached(engine: &Engine, table: &str) -> (bool, u64) {
    let table = engine.require_table(table).unwrap();
    (
        !table.doc_count_dirty.load(Ordering::Acquire),
        table.doc_count_cache.load(Ordering::Acquire),
    )
}

fn stored(engine: &Engine, table: &str) -> usize {
    let table = engine.require_table(table).unwrap();
    let count = table.document_store.read().len().unwrap();
    count
}

fn run(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap();
}

fn row_writes_keep_a_clean_count(engine: &Engine) {
    run(
        engine,
        "CREATE TABLE counted (id integer PRIMARY KEY, value integer)",
    );
    run(
        engine,
        "INSERT INTO counted SELECT g, g FROM generate_series(1, 20) AS g",
    );
    run(engine, "BEGIN");
    assert_eq!(engine.table_doc_count("counted").unwrap(), 20);
    assert_eq!(cached(engine, "counted"), (true, 20));
    run(engine, "INSERT INTO counted VALUES (21, 21), (22, 22)");
    assert_eq!(cached(engine, "counted"), (true, 22));
    run(engine, "UPDATE counted SET value = value + 1 WHERE id <= 5");
    assert_eq!(cached(engine, "counted"), (true, 22));
    // A changed primary key stores the row under another identity.
    run(engine, "UPDATE counted SET id = id + 100 WHERE id = 1");
    assert_eq!(cached(engine, "counted"), (true, 22));
    run(engine, "DELETE FROM counted WHERE id IN (2, 3, 4, 999)");
    assert_eq!(cached(engine, "counted"), (true, 19));
    assert_eq!(stored(engine, "counted"), 19);
    run(engine, "SAVEPOINT more");
    run(
        engine,
        "INSERT INTO counted SELECT g, g FROM generate_series(200, 204) AS g",
    );
    assert_eq!(engine.table_doc_count("counted").unwrap(), 24);
    run(engine, "ROLLBACK TO SAVEPOINT more");
    assert_eq!(engine.table_doc_count("counted").unwrap(), 19);
    assert_eq!(stored(engine, "counted"), 19);
    run(engine, "INSERT INTO counted VALUES (300, 300)");
    assert_eq!(engine.table_doc_count("counted").unwrap(), 20);
    run(engine, "ROLLBACK");
    assert_eq!(engine.table_doc_count("counted").unwrap(), 20);
    assert_eq!(stored(engine, "counted"), 20);
    run(engine, "DELETE FROM counted WHERE id > 15");
    assert_eq!(engine.table_doc_count("counted").unwrap(), 15);
    run(engine, "TRUNCATE counted");
    assert_eq!(engine.table_doc_count("counted").unwrap(), 0);
    assert_eq!(stored(engine, "counted"), 0);
}

#[test]
fn row_writes_keep_a_clean_count_equal_to_a_memory_store() {
    row_writes_keep_a_clean_count(&Engine::new());
}

#[test]
fn row_writes_keep_a_clean_count_equal_to_a_persistent_store() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("count.db")).unwrap();
    row_writes_keep_a_clean_count(&engine);
}

#[test]
fn a_failed_statement_leaves_no_count_ahead_of_the_store() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("failed.db")).unwrap();
    run(
        &engine,
        "CREATE TABLE counted (id integer PRIMARY KEY, value integer CHECK (value < 100))",
    );
    run(
        &engine,
        "INSERT INTO counted SELECT g, g FROM generate_series(1, 10) AS g",
    );
    assert_eq!(engine.table_doc_count("counted").unwrap(), 10);
    assert!(engine
        .sql(
            "INSERT INTO counted SELECT g, g FROM generate_series(95, 105) AS g",
            &[]
        )
        .is_err());
    assert_eq!(engine.table_doc_count("counted").unwrap(), 10);
    assert_eq!(stored(&engine, "counted"), 10);
    assert!(engine
        .sql("INSERT INTO counted VALUES (11, 11), (5, 5)", &[])
        .is_err());
    assert_eq!(engine.table_doc_count("counted").unwrap(), 10);
    assert_eq!(stored(&engine, "counted"), 10);
}

#[test]
fn a_row_write_changes_only_its_own_tables_count() {
    let engine = Engine::new();
    run(&engine, "CREATE TABLE parent (id integer, value integer)");
    run(&engine, "CREATE TABLE child () INHERITS (parent)");
    run(&engine, "INSERT INTO parent VALUES (1, 1), (2, 2)");
    run(&engine, "BEGIN");
    assert_eq!(engine.table_doc_count("parent").unwrap(), 2);
    assert_eq!(engine.table_doc_count("child").unwrap(), 0);
    run(&engine, "INSERT INTO child VALUES (3, 3)");
    assert_eq!(cached(&engine, "parent"), (true, 2));
    assert_eq!(cached(&engine, "child"), (true, 1));
    run(&engine, "COMMIT");
    assert_eq!(stored(&engine, "parent"), 2);
    assert_eq!(stored(&engine, "child"), 1);
}

#[test]
fn a_discarded_count_stays_discarded_and_an_impossible_one_is_discarded() {
    let engine = Engine::new();
    run(&engine, "CREATE TABLE counted (id integer PRIMARY KEY)");
    let table = engine.require_table("counted").unwrap();
    table.discard_document_count();
    table.doc_count_cache.store(7, Ordering::Release);
    table.apply_document_count_change(DocumentCountChange::Added);
    assert_eq!(cached(&engine, "counted"), (false, 7));
    assert_eq!(table.stored_document_count().unwrap(), 0);
    assert_eq!(cached(&engine, "counted"), (true, 0));
    table.apply_document_count_change(DocumentCountChange::Unchanged);
    assert_eq!(cached(&engine, "counted"), (true, 0));
    // No document is left to remove, so the count cannot be the store's.
    table.apply_document_count_change(DocumentCountChange::Removed);
    assert!(!cached(&engine, "counted").0);
}
