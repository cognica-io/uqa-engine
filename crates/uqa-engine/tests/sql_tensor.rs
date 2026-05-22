//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL coverage for `TENSOR(N)` chunk embeddings backed by IVF.

use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::{Engine, SQLParam};

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
