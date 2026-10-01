//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index-only reads: a query whose fields the table's indexes all hold is answered from index entries, and every other read still reads documents.

use super::index_columns::{assert_accelerators_equal_documents, create, held_fields, run};
use super::*;
use uqa_storage::{MemoryDocumentStore, ValueIndexKey};

/// The values of `column` in result order.
fn column(result: &uqa_sql::SQLResult, column: &str) -> Vec<Value> {
    result.rows.iter().map(|row| row[column].clone()).collect()
}

/// Replace every stored document's `b` and `c`, leaving the index entries as they were built, so a result tells which of the two a read projected.
fn diverge_documents(engine: &Engine) {
    let table = engine.table("covered").unwrap().expect("table");
    let ids = table.document_store.read().doc_ids().unwrap();
    let stored = table.document_store.read().get_stored_many(&ids).unwrap();
    let mut diverged = MemoryDocumentStore::new();
    for (id, mut document) in stored {
        document
            .fields_mut()
            .insert("b".into(), Value::Str("document".into()));
        document.fields_mut().insert("c".into(), Value::Int(-1));
        diverged.put_stored(id, document).unwrap();
    }
    *table.document_store.write() = Box::new(diverged);
}

#[test]
fn a_query_of_indexed_fields_projects_index_entries() {
    let engine = Engine::new();
    create(&engine);
    run(&engine, "CREATE INDEX covered_a ON covered (a) INCLUDE (b)");
    diverge_documents(&engine);

    assert_eq!(
        column(
            &run(&engine, "SELECT b FROM covered WHERE a = 5 ORDER BY b"),
            "b"
        ),
        ["b15", "b25", "b35", "b5"].map(s)
    );
    assert_eq!(
        column(
            &run(&engine, "SELECT id, a, b FROM covered WHERE id = 7"),
            "b"
        ),
        [s("b7")]
    );
    let counted = run(&engine, "SELECT count(*) AS n FROM covered WHERE a = 5");
    assert_eq!(column(&counted, "n"), [Value::Int(4)]);

    // `c` is in no index, so its rows are read from the documents.
    assert_eq!(
        column(
            &run(&engine, "SELECT b, c FROM covered WHERE a = 5 ORDER BY id"),
            "b"
        ),
        vec![s("document"); 4]
    );
    // A locking read returns the current documents.
    assert_eq!(
        column(
            &run(&engine, "SELECT b FROM covered WHERE a = 5 FOR UPDATE"),
            "b"
        ),
        vec![s("document"); 4]
    );
    run(&engine, "SET enable_indexonlyscan = off");
    assert_eq!(
        column(&run(&engine, "SELECT b FROM covered WHERE a = 5"), "b"),
        vec![s("document"); 4]
    );
    run(&engine, "RESET enable_indexonlyscan");
    assert_eq!(
        column(
            &run(&engine, "SELECT b FROM covered WHERE a = 5 ORDER BY b"),
            "b"
        ),
        ["b15", "b25", "b35", "b5"].map(s)
    );
}

#[test]
fn every_plain_key_column_of_an_index_is_held() {
    let engine = Engine::new();
    create(&engine);
    run(&engine, "CREATE INDEX covered_ac ON covered (a, c)");
    diverge_documents(&engine);
    assert_eq!(
        column(
            &run(&engine, "SELECT c FROM covered WHERE a = 5 ORDER BY c"),
            "c"
        ),
        [10, 30, 50, 70].map(Value::Int)
    );
    let table = engine.table("covered").unwrap().expect("table");
    let indexes = table.value_indexes.read();
    assert!(!indexes[&ValueIndexKey::Column("c".into())].is_carried());
}

/// Run `sql` as an index-only read and as a read of the documents, and return the rows when both agree.
fn agreed(engine: &Engine, sql: &str) -> Vec<Vec<Value>> {
    let rows = |result: uqa_sql::SQLResult| {
        result
            .rows
            .iter()
            .map(|row| {
                result
                    .columns
                    .iter()
                    .map(|name| row[name.as_str()].clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let indexed = rows(run(engine, sql));
    run(engine, "SET enable_indexonlyscan = off");
    let documents = rows(run(engine, sql));
    run(engine, "RESET enable_indexonlyscan");
    assert_eq!(indexed, documents, "{sql}");
    indexed
}

fn writes_keep_index_entries_equal_to_documents(engine: &Engine) {
    create(engine);
    run(
        engine,
        "CREATE INDEX covered_a ON covered (a) INCLUDE (b, c)",
    );
    let queries = [
        "SELECT a, b, c FROM covered WHERE a = 5 ORDER BY b",
        "SELECT b FROM covered WHERE a BETWEEN 2 AND 3 ORDER BY b",
        "SELECT sum(c) AS total, count(*) AS n FROM covered WHERE a IN (1, 5, 9)",
        "SELECT id, c FROM covered WHERE id BETWEEN 10 AND 20 ORDER BY id",
        "SELECT b FROM covered WHERE a IS NULL ORDER BY b",
    ];
    let check = |engine: &Engine| {
        for sql in queries {
            agreed(engine, sql);
        }
        assert_accelerators_equal_documents(engine, "covered");
    };
    check(engine);
    run(
        engine,
        "INSERT INTO covered VALUES (100, 5, 'inserted', 500), (101, NULL, NULL, NULL)",
    );
    check(engine);
    assert_eq!(
        agreed(engine, "SELECT b, c FROM covered WHERE a IS NULL"),
        [[Value::Null, Value::Null]]
    );
    run(
        engine,
        "UPDATE covered SET b = 'updated', c = c + 1 WHERE a = 5",
    );
    check(engine);
    run(engine, "UPDATE covered SET a = 3 WHERE id IN (5, 15)");
    check(engine);
    run(engine, "DELETE FROM covered WHERE id IN (25, 100)");
    check(engine);

    run(engine, "BEGIN");
    run(engine, "UPDATE covered SET b = 'private' WHERE a = 2");
    run(engine, "INSERT INTO covered VALUES (200, 2, 'own', 0)");
    check(engine);
    run(engine, "SAVEPOINT nested");
    run(engine, "DELETE FROM covered WHERE a = 2");
    check(engine);
    run(engine, "ROLLBACK TO SAVEPOINT nested");
    check(engine);
    run(engine, "ROLLBACK");
    check(engine);
    assert_eq!(
        agreed(engine, "SELECT count(*) AS n FROM covered WHERE a = 2"),
        [[Value::Int(4)]]
    );

    run(engine, "TRUNCATE covered");
    check(engine);
    run(engine, "INSERT INTO covered VALUES (1, 5, 'again', 7)");
    assert_eq!(
        agreed(engine, "SELECT b, c FROM covered WHERE a = 5"),
        [[s("again"), Value::Int(7)]]
    );
}

#[test]
fn writes_keep_index_entries_equal_to_memory_documents() {
    writes_keep_index_entries_equal_to_documents(&Engine::new());
}

#[test]
fn writes_keep_index_entries_equal_to_persistent_documents() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("covered.db")).unwrap();
    writes_keep_index_entries_equal_to_documents(&engine);
}

#[test]
fn a_reopened_database_loads_carried_columns_from_their_postings() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reopened.db");
    {
        let engine = Engine::open(&path).unwrap();
        create(&engine);
        run(&engine, "CREATE INDEX covered_a ON covered (a) INCLUDE (b)");
        run(&engine, "UPDATE covered SET b = 'changed' WHERE id = 15");
    }
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
    assert_eq!(postings("b"), 40);
    assert_eq!(postings("c"), 0);

    let engine = Engine::open(&path).unwrap();
    assert_eq!(held_fields(&engine, "covered"), []);
    assert_eq!(
        agreed(&engine, "SELECT b FROM covered WHERE a = 5 ORDER BY b"),
        [[s("b25")], [s("b35")], [s("b5")], [s("changed")]]
    );
    // The read loaded what it projected and what its predicate searched.
    assert_eq!(
        held_fields(&engine, "covered"),
        [("a".into(), false), ("b".into(), true)]
    );
    run(&engine, "DROP INDEX covered_a");
    drop(engine);
    assert_eq!(postings("b"), 0);
}

#[test]
fn index_entries_reproduce_every_stored_type() {
    let engine = Engine::new();
    run(
        &engine,
        "CREATE TABLE typed (
            k integer PRIMARY KEY,
            small smallint, big bigint, fraction double precision, exact numeric(12, 4),
            flag boolean, word text, padded char(6), limited varchar(8), raw bytea,
            day date, moment timestamp, zoned timestamptz, span interval,
            identifier uuid, document jsonb, plain json, numbers integer[], words text[]
        )",
    );
    run(
        &engine,
        "INSERT INTO typed VALUES
            (1, 7, 9007199254740993, 1.5, 12.3400, true, 'text', 'ab', 'limit', '\\x00ff10',
             '2026-10-01', '2026-10-01 12:34:56.789', '2026-10-01 12:34:56+09', '1 day 02:03:04',
             'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '{\"k\": [1, 2]}', '{\"k\":  1}', '{1,2,3}', '{a,NULL,c}'),
            (2, -1, -1, 'NaN', 0.0001, false, '', '', '', '\\x',
             '0001-01-01', '1970-01-01 00:00:00', '1970-01-01 00:00:00+00', '-1 mon',
             '00000000-0000-0000-0000-000000000000', 'null', 'null', '{}', '{}'),
            (3, NULL, NULL, '-Infinity', NULL, NULL, NULL, NULL, NULL, NULL,
             NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
            (4, 0, 0, -0.0, -0.5, true, repeat('x', 5000), 'abcdef', 'abcdefgh', decode(repeat('ab', 3000), 'hex'),
             '9999-12-31', '1999-01-08 04:05:06', '1999-01-08 04:05:06-08', '1 year 2 mons 3 days',
             'ffffffff-ffff-ffff-ffff-ffffffffffff', '[]', '[]', '{{1,2},{3,4}}', '{\"quoted, value\"}')",
    );
    run(
        &engine,
        "CREATE INDEX typed_k ON typed (k) INCLUDE (
            small, big, fraction, exact, flag, word, padded, limited, raw, day, moment, zoned, span,
            identifier, document, plain, numbers, words)",
    );
    let rows = agreed(
        &engine,
        "SELECT * FROM typed WHERE k BETWEEN 1 AND 4 ORDER BY k",
    );
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].len(), 19);
    // The read projected index entries: every column of the table has one.
    assert_eq!(held_fields(&engine, "typed").len(), 19);
}

#[test]
fn partial_and_expression_indexes_hold_their_plain_columns_for_every_row() {
    let engine = Engine::new();
    create(&engine);
    run(
        &engine,
        "CREATE INDEX covered_partial ON covered (a) INCLUDE (b) WHERE c > 40",
    );
    run(
        &engine,
        "CREATE INDEX covered_expression ON covered ((a + 1), c)",
    );
    // A row outside the partial index's predicate is still held.
    assert_eq!(
        agreed(&engine, "SELECT b, c FROM covered WHERE a = 5 ORDER BY c"),
        [
            [s("b5"), Value::Int(10)],
            [s("b15"), Value::Int(30)],
            [s("b25"), Value::Int(50)],
            [s("b35"), Value::Int(70)],
        ]
    );
    let held = held_fields(&engine, "covered");
    assert!(held.contains(&("b".into(), true)));
    assert!(held.contains(&("c".into(), false)));
}
