//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Every plain key column and every included column of a btree index has an accelerator that holds the stored value of each row.

use super::*;
use uqa_storage::ValueIndexKey;

pub(super) fn run(engine: &Engine, sql: &str) -> uqa_sql::SQLResult {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

pub(super) fn create(engine: &Engine) {
    run(
        engine,
        "CREATE TABLE covered (id integer PRIMARY KEY, a integer, b text, c integer)",
    );
    run(
        engine,
        "INSERT INTO covered SELECT g, g % 10, 'b' || g, g * 2 FROM generate_series(1, 40) AS g",
    );
}

/// Each accelerator of `table` with whether it only carries its column.
pub(super) fn held_fields(engine: &Engine, table: &str) -> Vec<(String, bool)> {
    let table = engine.table(table).unwrap().expect("table");
    let indexes = table.value_indexes.read();
    indexes
        .iter()
        .map(|(field, index)| (field.name().to_owned(), index.is_carried()))
        .collect()
}

/// Every column accelerator of `table` holds exactly the stored field of every document, and nothing else.
pub(super) fn assert_accelerators_equal_documents(engine: &Engine, table: &str) {
    let state = engine.table(table).unwrap().expect("table");
    let store = state.document_store.read();
    let ids = store.doc_ids().unwrap();
    let documents = store.get_stored_many(&ids).unwrap();
    for (field, index) in state.value_indexes.read().iter() {
        let ValueIndexKey::Column(column) = field else {
            continue;
        };
        for id in &ids {
            let stored = documents[id].fields().get(column).unwrap_or(&Value::Null);
            assert_eq!(
                index.stored_value(*id),
                Some(stored),
                "{table}.{column} of document {id}"
            );
        }
        for id in [0, 41, 99, 500, 1000] {
            assert_eq!(
                index.contains(id),
                documents.contains_key(&id),
                "{table}.{column} of document {id}"
            );
        }
    }
}

#[test]
fn every_plain_key_column_is_searched_and_every_included_column_is_carried() {
    let engine = Engine::new();
    create(&engine);
    run(
        &engine,
        "CREATE INDEX covered_ac ON covered (a, c) INCLUDE (b, a)",
    );
    assert_eq!(
        held_fields(&engine, "covered"),
        [
            ("a".into(), false),
            ("b".into(), true),
            ("c".into(), false),
            ("id".into(), false)
        ]
    );
    assert_accelerators_equal_documents(&engine, "covered");
    // The trailing key column answers a predicate of its own.
    assert_eq!(
        run(&engine, "SELECT id FROM covered WHERE c = 14").rows[0]["id"],
        Value::Int(7)
    );
}

#[test]
fn an_included_column_is_carried_until_an_index_makes_it_a_key() {
    let engine = Engine::new();
    create(&engine);
    run(&engine, "CREATE INDEX covered_a ON covered (a) INCLUDE (b)");
    let carried = |engine: &Engine| {
        held_fields(engine, "covered")
            .into_iter()
            .find(|(field, _)| field == "b")
            .map(|(_, carried)| carried)
    };
    assert_eq!(carried(&engine), Some(true));
    run(&engine, "CREATE INDEX covered_b ON covered (b)");
    assert_eq!(carried(&engine), Some(false));
    assert_eq!(
        run(&engine, "SELECT id FROM covered WHERE b = 'b7'").rows[0]["id"],
        Value::Int(7)
    );
    assert_accelerators_equal_documents(&engine, "covered");
    run(&engine, "DROP INDEX covered_b");
    assert_eq!(carried(&engine), Some(true));
    assert_accelerators_equal_documents(&engine, "covered");
    run(&engine, "DROP INDEX covered_a");
    assert_eq!(carried(&engine), None);
}

#[test]
fn writes_and_rollbacks_keep_accelerators_equal_to_documents() {
    let engine = Engine::new();
    create(&engine);
    run(
        &engine,
        "CREATE INDEX covered_a ON covered (a) INCLUDE (b, c)",
    );
    let check = || assert_accelerators_equal_documents(&engine, "covered");
    check();
    run(
        &engine,
        "INSERT INTO covered VALUES (100, 5, 'inserted', 500), (101, NULL, NULL, NULL)",
    );
    check();
    run(
        &engine,
        "UPDATE covered SET b = 'updated', c = c + 1 WHERE a = 5",
    );
    check();
    run(&engine, "UPDATE covered SET id = id + 400 WHERE id = 7");
    check();
    run(&engine, "DELETE FROM covered WHERE id IN (25, 100)");
    check();
    run(&engine, "BEGIN");
    run(&engine, "UPDATE covered SET b = 'private' WHERE a = 2");
    run(&engine, "SAVEPOINT nested");
    run(&engine, "DELETE FROM covered WHERE a = 3");
    check();
    run(&engine, "ROLLBACK TO SAVEPOINT nested");
    // A rollback drops the column accelerators of a memory table, and the next statement that needs one builds it.
    run(&engine, "SELECT count(*) FROM covered WHERE a = 2");
    check();
    run(&engine, "ROLLBACK");
    run(&engine, "SELECT count(*) FROM covered WHERE a = 2");
    check();
    run(&engine, "ALTER TABLE covered RENAME COLUMN b TO label");
    assert!(held_fields(&engine, "covered").contains(&("label".into(), true)));
    check();
    run(&engine, "TRUNCATE covered");
    check();
}

#[test]
fn included_columns_keep_durable_postings() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("postings.db");
    let postings = |field: &str| -> i64 {
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM _btree_index_entries WHERE table_name = 'public.covered' AND field = ?1",
                [field],
                |row| row.get(0),
            )
            .unwrap()
    };
    {
        let engine = Engine::open(&path).unwrap();
        create(&engine);
        run(
            &engine,
            "CREATE INDEX covered_ac ON covered (a, c) INCLUDE (b)",
        );
        run(&engine, "INSERT INTO covered VALUES (41, 1, 'more', 82)");
        run(&engine, "DELETE FROM covered WHERE id IN (1, 2)");
    }
    for field in ["a", "b", "c", "id"] {
        assert_eq!(postings(field), 39, "{field}");
    }
    let engine = Engine::open(&path).unwrap();
    run(&engine, "DROP INDEX covered_ac");
    drop(engine);
    for field in ["a", "b", "c"] {
        assert_eq!(postings(field), 0, "{field}");
    }
    assert_eq!(postings("id"), 39);
}
