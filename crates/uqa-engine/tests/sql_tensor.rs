//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL coverage for `TENSOR(N)` chunk embeddings backed by IVF and HNSW.

use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::{Engine, SQLParam};

use rusqlite::{params, Connection};
use uqa_storage::RelationIdentity;

fn int_column(row: &uqa_sql::ResultRow, column: &str) -> i64 {
    match row.get(column) {
        Some(Value::Int(value)) => *value,
        other => panic!("expected integer column {column}, got {other:?}"),
    }
}

fn float_column(row: &uqa_sql::ResultRow, column: &str) -> f64 {
    match row.get(column) {
        Some(Value::Float(value)) => *value,
        other => panic!("expected float column {column}, got {other:?}"),
    }
}

#[test]
fn tensor_ivf_knn_scores_best_chunk_per_row() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, chunks TENSOR(2))",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (id, body, chunks) VALUES \
             (1, 'alpha body', ARRAY[ARRAY[1.0, 0.0], ARRAY[0.0, 1.0]]), \
             (2, 'beta body', ARRAY[ARRAY[0.2, 0.8]]), \
             (3, 'gamma body', ARRAY[ARRAY[-1.0, 0.0]])",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX docs_chunks_ivf ON docs USING ivf (chunks) \
             WITH (lists = 2, probes = 2, train_threshold = 2)",
            &[],
        )
        .unwrap();

    let result = engine
        .sql(
            "SELECT id, body, _score FROM docs \
             WHERE knn_match(chunks, ARRAY[0.0, 1.0], 2) \
             ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = result
        .rows
        .iter()
        .map(|row| int_column(row, "id"))
        .collect();
    assert_eq!(ids, vec![1, 2]);
    assert_eq!(
        result.rows[0].get("body"),
        Some(&Value::Str("alpha body".into()))
    );
    assert!((float_column(&result.rows[0], "_score") - 1.0).abs() < 1e-6);
    assert!(float_column(&result.rows[1], "_score") < 1.0);
}

#[test]
fn tensor_hnsw_persists_and_scores_each_row_once() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("tensor-hnsw.db");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE TABLE docs (id INTEGER PRIMARY KEY, chunks TENSOR(2))",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO docs (id, chunks) VALUES \
                 (1, ARRAY[ARRAY[1.0, 0.0], ARRAY[0.0, 1.0]]), \
                 (2, ARRAY[ARRAY[0.2, 0.8]]), \
                 (3, ARRAY[ARRAY[-1.0, 0.0]])",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE INDEX docs_chunks_hnsw ON docs USING hnsw (chunks) \
                 WITH (m = 4, ef_construction = 24, ef_search = 16, seed = 7)",
                &[],
            )
            .unwrap();
    }

    let engine = Engine::open(&database).unwrap();
    let result = engine
        .sql(
            "SELECT id, _score FROM docs \
             WHERE knn_match(chunks, ARRAY[0.0, 1.0], 2) \
             ORDER BY _score DESC",
            &[],
        )
        .unwrap();
    let ids = result
        .rows
        .iter()
        .map(|row| int_column(row, "id"))
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![1, 2]);
    assert!((float_column(&result.rows[0], "_score") - 1.0).abs() < 1e-6);
    let connection = Connection::open(&database).unwrap();
    let node_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM _hnsw_nodes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(node_count, 4);
}

#[test]
fn tensor_ivf_backfill_trains_on_all_chunk_vectors_not_tensor_rows() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("tensor-threshold.db");
    {
        let engine = Engine::open(&db).unwrap();
        engine
            .sql(
                "CREATE TABLE docs (id INTEGER PRIMARY KEY, chunks TENSOR(2))",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO docs (id, chunks) VALUES \
                 (1, ARRAY[ARRAY[1.0, 0.0], ARRAY[0.0, 1.0]])",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE INDEX docs_chunks_ivf ON docs USING ivf (chunks) \
                 WITH (lists = 4, probes = 4, train_threshold = 2)",
                &[],
            )
            .unwrap();
    }

    let conn = Connection::open(&db).unwrap();
    let table = RelationIdentity::from_legacy_name("docs")
        .unwrap()
        .qualified_name();
    let (state, trained_size, vector_count, train_threshold): (String, i64, i64, i64) = conn
        .query_row(
            "SELECT state, trained_size, vector_count, train_threshold
               FROM _ivf_indexes
              WHERE table_name = ?1 AND field = ?2",
            params![table, "chunks"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(state, "trained");
    assert_eq!(trained_size, 2);
    assert_eq!(vector_count, 2);
    assert_eq!(train_threshold, 2);
    let assignments: i64 = conn
        .query_row(
            "SELECT COUNT(*)
               FROM _ivf_assignments
              WHERE table_name = ?1 AND field = ?2",
            params![table, "chunks"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(assignments, 2);
}

#[test]
fn tensor_knn_returns_one_result_per_row_even_with_many_chunks() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, chunks TENSOR(2))",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs (id, chunks) VALUES \
             (1, ARRAY[ARRAY[1.0, 0.0], ARRAY[0.9, 0.1]]), \
             (2, ARRAY[ARRAY[0.8, 0.2]]), \
             (3, ARRAY[ARRAY[0.0, 1.0]])",
            &[],
        )
        .unwrap();

    let result = engine
        .sql(
            "SELECT id FROM docs \
             WHERE knn_match(chunks, ARRAY[1.0, 0.0], 3) \
             ORDER BY id",
            &[],
        )
        .unwrap();
    let ids: Vec<i64> = result
        .rows
        .iter()
        .map(|row| int_column(row, "id"))
        .collect();
    assert_eq!(ids, vec![1, 2, 3]);
}

#[test]
fn tensor_dimension_mismatch_is_rejected() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE docs (id INTEGER PRIMARY KEY, chunks TENSOR(2))",
            &[],
        )
        .unwrap();
    let err = engine
        .sql(
            "INSERT INTO docs (id, chunks) VALUES (1, ARRAY[ARRAY[1.0, 0.0, 0.0]])",
            &[],
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("vector dimension mismatch"), "{err}");
}

#[test]
fn nullable_vector_and_tensor_values_have_no_index_entries() {
    let dir = tempdir().unwrap();
    let engine = Engine::open(&dir.path().join("nullable-vectors.db")).unwrap();
    engine
        .sql(
            "CREATE TABLE docs (\
               id INTEGER PRIMARY KEY, \
               embedding VECTOR(2), \
               chunks TENSOR(2)\
             )",
            &[],
        )
        .unwrap();

    engine
        .sql(
            "INSERT INTO docs (id, embedding, chunks) VALUES (1, NULL, NULL)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "UPDATE docs SET embedding = ARRAY[1.0, 0.0], \
                             chunks = ARRAY[ARRAY[0.0, 1.0]] \
             WHERE id = 1",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "UPDATE docs SET embedding = NULL, chunks = NULL WHERE id = 1",
            &[],
        )
        .unwrap();

    let row = engine
        .sql("SELECT embedding, chunks FROM docs WHERE id = 1", &[])
        .unwrap()
        .rows
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(row.get("embedding"), Some(&Value::Null));
    assert_eq!(row.get("chunks"), Some(&Value::Null));
    assert!(engine
        .sql(
            "SELECT id FROM docs WHERE knn_match(embedding, ARRAY[1.0, 0.0], 1)",
            &[],
        )
        .unwrap()
        .rows
        .is_empty());
    assert!(engine
        .sql(
            "SELECT id FROM docs WHERE knn_match(chunks, ARRAY[0.0, 1.0], 1)",
            &[],
        )
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn tensor_sql_param_and_sqlite_reopen_preserve_ivf_indexing() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("tensor.db");
    {
        let engine = Engine::open(&db).unwrap();
        engine
            .sql(
                "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, chunks TENSOR(2))",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO docs (id, body, chunks) VALUES ($1, $2, $3)",
                &[
                    SQLParam::scalar(Value::Int(1)),
                    SQLParam::scalar(Value::Str("persistent body".into())),
                    SQLParam::tensor(vec![vec![0.0, 1.0], vec![1.0, 0.0]]),
                ],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO docs (id, body, chunks) VALUES \
                 (2, 'other body', ARRAY[ARRAY[-1.0, 0.0]])",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE INDEX docs_chunks_ivf ON docs USING ivf (chunks) \
                 WITH (lists = 2, probes = 2, train_threshold = 2)",
                &[],
            )
            .unwrap();
    }

    let engine = Engine::open(&db).unwrap();
    let result = engine
        .sql(
            "SELECT id, body, _score FROM docs \
             WHERE knn_match(chunks, ARRAY[0.0, 1.0], 1)",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(int_column(&result.rows[0], "id"), 1);
    assert_eq!(
        result.rows[0].get("body"),
        Some(&Value::Str("persistent body".into()))
    );
    assert!((float_column(&result.rows[0], "_score") - 1.0).abs() < 1e-6);
}
