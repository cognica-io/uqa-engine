//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A filter on `_doc_id` or `_meta.doc_id` compares each row's document identity, whatever the table's key is and whatever else the query projects or filters on. A table column named `_doc_id` keeps its own meaning.

use uqa_core::Value;
use uqa_engine::Engine;

const UNMAPPED: i64 = 1 << 62;

fn texts(engine: &Engine, query: &str, column: &str) -> Vec<String> {
    engine
        .sql(query, &[])
        .unwrap_or_else(|error| panic!("{query}: {error}"))
        .rows
        .iter()
        .map(|row| match row.get(column) {
            Some(Value::Int(value)) => value.to_string(),
            Some(Value::Str(value)) => value.clone(),
            other => panic!("{query}: unexpected value {other:?}"),
        })
        .collect()
}

fn run(engine: &Engine, statement: &str) {
    engine
        .sql(statement, &[])
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

fn check(engine: &Engine, label: &str) {
    run(
        engine,
        "CREATE TABLE keyed (id bigint PRIMARY KEY, v integer);
         INSERT INTO keyed VALUES (1, 1), (2, 2), (3, 3), (-1, 4);
         CREATE TABLE plain (k integer, v integer);
         INSERT INTO plain VALUES (10, 1), (20, 2);
         CREATE TABLE words (k text PRIMARY KEY, v integer);
         INSERT INTO words VALUES ('a', 1), ('b', 2)",
    );
    // The negative key's identity lies above every key-named one.
    for (query, column, expected) in [
        ("SELECT id FROM keyed WHERE _doc_id = -1", "id", vec![]),
        (
            "SELECT id FROM keyed WHERE _doc_id = 4611686018427387904",
            "id",
            vec!["-1"],
        ),
        ("SELECT id FROM keyed WHERE _doc_id = 2", "id", vec!["2"]),
        ("SELECT id FROM keyed WHERE _doc_id > 3", "id", vec!["-1"]),
        (
            "SELECT id FROM keyed WHERE _doc_id IN (1, 3, 99) ORDER BY id",
            "id",
            vec!["1", "3"],
        ),
        (
            "SELECT id FROM keyed WHERE _meta.doc_id > 3",
            "id",
            vec!["-1"],
        ),
        (
            "SELECT id FROM keyed WHERE _meta.doc_id = 2",
            "id",
            vec!["2"],
        ),
        ("SELECT k FROM plain WHERE _doc_id = 2", "k", vec!["20"]),
        (
            "SELECT p.k FROM plain p WHERE p._doc_id = 2",
            "k",
            vec!["20"],
        ),
        ("SELECT k FROM words WHERE _doc_id > 1", "k", vec!["b"]),
        // Projecting the identity leaves a filter on another column intact.
        (
            "SELECT _doc_id FROM plain WHERE v = 2",
            "_doc_id",
            vec!["2"],
        ),
        (
            "SELECT k FROM plain WHERE v = 2 AND _doc_id = 2",
            "k",
            vec!["20"],
        ),
    ] {
        assert_eq!(texts(engine, query, column), expected, "{label}: {query}");
    }
    assert_eq!(
        texts(engine, "SELECT _doc_id FROM keyed WHERE v = 4", "_doc_id"),
        [UNMAPPED.to_string()],
        "{label}"
    );
    assert_eq!(
        texts(
            engine,
            "UPDATE plain SET v = 5 WHERE _doc_id = 1 RETURNING k",
            "k"
        ),
        ["10"],
        "{label}"
    );
    assert_eq!(
        texts(
            engine,
            "UPDATE plain SET v = v WHERE _doc_id IN (1, 7) AND tableoid = 'plain'::regclass RETURNING k",
            "k"
        ),
        ["10"],
        "{label}"
    );
    assert_eq!(
        texts(
            engine,
            "DELETE FROM plain WHERE _doc_id = 2 RETURNING k",
            "k"
        ),
        ["20"],
        "{label}"
    );
    assert_eq!(
        texts(engine, "SELECT k FROM plain WHERE v = 5", "k"),
        ["10"],
        "{label}"
    );
    // A stored column named `_doc_id` is the column, not the identity, which `_meta.doc_id` still names.
    run(
        engine,
        "CREATE TABLE named (_doc_id integer, v integer);
         INSERT INTO named VALUES (100, 1), (200, 2)",
    );
    for (query, expected) in [
        ("SELECT v FROM named WHERE _doc_id = 100", "1"),
        ("SELECT v FROM named WHERE _doc_id = 2", ""),
        ("SELECT v FROM named WHERE _meta.doc_id = 2", "2"),
    ] {
        assert_eq!(
            texts(engine, query, "v").join(","),
            expected,
            "{label}: {query}"
        );
    }
}

#[test]
fn doc_id_filters_compare_the_identity_of_each_row() {
    check(&Engine::new(), "memory");
    let directory = tempfile::tempdir().unwrap();
    check(
        &Engine::open(&directory.path().join("row-metadata-filters.db")).unwrap(),
        "native",
    );
}
