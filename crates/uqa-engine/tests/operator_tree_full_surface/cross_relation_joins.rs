//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cross-relation SQL operator-join coverage.

use std::collections::BTreeSet;

use super::{fixture, Engine, Value};

fn add_archive_docs(engine: &Engine) {
    engine
        .sql(
            "CREATE TABLE archive_docs (\
                id INTEGER PRIMARY KEY, \
                headline TEXT, \
                archive_category TEXT, \
                archive_marker TEXT, \
                archive_embedding VECTOR(2)\
            )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX archive_docs_fts ON archive_docs USING gin (headline)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO archive_docs \
                 (id, headline, archive_category, archive_marker, archive_embedding) VALUES \
             (2, 'graph two', 'A', 'graph', ARRAY[0.0, 1.0]), \
             (3, 'graph three', 'B', 'graph', ARRAY[0.0, 1.0]), \
             (101, 'rust async', 'A', 'search', ARRAY[1.0, 0.0]), \
             (102, 'rust systems', 'A', 'search', ARRAY[0.9, 0.1])",
            &[],
        )
        .unwrap();
}

fn operator_join_pairs(result: &uqa_sql::SQLResult) -> BTreeSet<(i64, i64)> {
    result
        .rows
        .iter()
        .map(|row| {
            let Some(Value::Int(left)) = row.get("left_doc_id") else {
                panic!("operator join row has no left_doc_id: {row:?}");
            };
            let Some(Value::Int(right)) = row.get("right_doc_id") else {
                panic!("operator join row has no right_doc_id: {row:?}");
            };
            (*left, *right)
        })
        .collect()
}

#[test]
fn operator_join_table_functions_execute_each_operand_on_its_relation() {
    let engine = fixture();
    add_archive_docs(&engine);
    let cases = [
        (
            "text_similarity_join",
            "SELECT left_doc_id, right_doc_id \
             FROM text_similarity_join(\
                 docs,\
                 text_match(title, 'rust'),\
                 archive_docs,\
                 text_match(headline, 'rust'),\
                 0.2\
             )",
            BTreeSet::from([(1, 101), (1, 102), (2, 101), (2, 102)]),
        ),
        (
            "vector_similarity_join",
            "SELECT left_doc_id, right_doc_id \
             FROM vector_similarity_join(\
                 docs,\
                 knn_match(embedding, ARRAY[1.0, 0.0], 2),\
                 archive_docs,\
                 knn_match(archive_embedding, ARRAY[1.0, 0.0], 2),\
                 0.8\
             )",
            BTreeSet::from([(1, 101), (1, 102), (2, 101), (2, 102)]),
        ),
        (
            "graph_join",
            "SELECT left_doc_id, right_doc_id \
             FROM graph_join(\
                 docs,\
                 graph_pagerank('social'),\
                 archive_docs,\
                 archive_marker = 'graph',\
                 'follows',\
                 'social'\
             )",
            BTreeSet::from([(1, 2), (2, 3)]),
        ),
        (
            "hybrid_join",
            "SELECT left_doc_id, right_doc_id \
             FROM hybrid_join(\
                 docs,\
                 category = 'A' AND knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                 archive_docs,\
                 archive_category = 'A' AND archive_marker = 'search' \
                     AND knn_match(archive_embedding, ARRAY[1.0, 0.0], 4)\
             )",
            BTreeSet::from([(1, 101), (1, 102), (2, 101), (2, 102)]),
        ),
        (
            "cross_paradigm_join",
            "SELECT left_doc_id, right_doc_id \
             FROM cross_paradigm_join(\
                 docs,\
                 graph_pagerank('social'),\
                 archive_docs,\
                 archive_category IS NOT NULL AND archive_marker = 'search'\
             )",
            BTreeSet::from([(1, 101), (1, 102), (2, 101), (2, 102)]),
        ),
    ];

    for (name, sql, expected) in cases {
        let result = engine.sql(sql, &[]).unwrap_or_else(|error| {
            panic!("{name} cross-relation execution failed: {error}");
        });
        assert_eq!(operator_join_pairs(&result), expected, "{name}");
    }
}

#[test]
fn both_operator_join_relations_are_tracked_as_view_dependencies() {
    let engine = fixture();
    add_archive_docs(&engine);
    engine
        .sql(
            "CREATE VIEW doc_pairs AS \
             SELECT left_doc_id, right_doc_id \
             FROM vector_similarity_join(\
                 docs,\
                 knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                 archive_docs,\
                 knn_match(archive_embedding, ARRAY[1.0, 0.0], 2),\
                 0.8\
             )",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT left_doc_id FROM doc_pairs", &[])
            .unwrap()
            .rows
            .len(),
        4
    );

    for table in ["docs", "archive_docs"] {
        let error = engine.sql(&format!("DROP TABLE {table}"), &[]).unwrap_err();
        assert!(error.to_string().contains("public.doc_pairs"), "{error}");
    }
}
