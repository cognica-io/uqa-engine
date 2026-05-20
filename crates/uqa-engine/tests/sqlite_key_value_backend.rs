//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine coverage for the SQLite-backed `KeyValue` storage path.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value as JSONValue;
use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::{Engine, ScoringMode};
use uqa_storage::{CatalogFacade, PersistentStorageBackend};
use uqa_storage_sqlite::SQLiteKeyValueStorage;

#[derive(Deserialize)]
struct Fixture {
    #[allow(dead_code)]
    version: u32,
    schema_sql: Vec<String>,
    data_sql: Vec<String>,
    cases: Vec<GoldenCase>,
}

#[derive(Deserialize)]
struct GoldenCase {
    name: String,
    sql: String,
    expected: Vec<JSONValue>,
}

fn open_key_value_engine(path: &std::path::Path) -> Engine {
    let storage = SQLiteKeyValueStorage::open(path).expect("open SQLite KeyValue storage");
    let catalog: Arc<dyn CatalogFacade> = Arc::new(storage.catalog());
    let backend: Arc<dyn PersistentStorageBackend> = Arc::new(storage.backend());
    Engine::from_persistent_backends(catalog, backend).expect("open KeyValue engine")
}

fn fixture_path() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests")
        .join("parity")
        .join("sql_golden_fixture.json")
}

fn load_fixture() -> Fixture {
    let bytes = std::fs::read(fixture_path()).expect("fixture present");
    serde_json::from_slice(&bytes).expect("fixture parses")
}

fn json_to_value(v: &JSONValue) -> Value {
    match v {
        JSONValue::Null | JSONValue::Object(_) => Value::Null,
        JSONValue::Bool(b) => Value::Bool(*b),
        JSONValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        JSONValue::String(s) => Value::Str(s.clone()),
        JSONValue::Array(items) => Value::List(items.iter().map(json_to_value).collect()),
    }
}

#[test]
fn engine_runs_text_and_vector_workloads_on_sqlite_key_value_storage() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("keyvalue-engine.sqlite3");

    {
        let engine = open_key_value_engine(&path);
        engine
            .sql(
                "CREATE TABLE articles (
                    id INTEGER PRIMARY KEY,
                    title TEXT,
                    embedding VECTOR(2)
                )",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE INDEX articles_title_gin ON articles USING gin (title)",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO articles (id, title, embedding) VALUES
                 (1, 'rust search engine', ARRAY[1.0, 0.0]),
                 (2, 'sqlite key value', ARRAY[0.0, 1.0])",
                &[],
            )
            .unwrap();

        let hits = engine.search("articles", "title", "rust", &ScoringMode::default(), 10);
        assert_eq!(hits.first().map(|hit| hit.doc_id), Some(1));
        let vector_hits = engine.knn_search("articles", "embedding", vec![1.0, 0.0], 1);
        assert_eq!(vector_hits.first().map(|hit| hit.doc_id), Some(1));
    }

    let reopened = open_key_value_engine(&path);
    let got = reopened.get_document("articles", 2).unwrap();
    assert_eq!(
        got.get("title"),
        Some(&uqa_core::Value::Str("sqlite key value".into()))
    );
    let hits = reopened.search("articles", "title", "sqlite", &ScoringMode::default(), 10);
    assert_eq!(hits.first().map(|hit| hit.doc_id), Some(2));
    let vector_hits = reopened.knn_search("articles", "embedding", vec![0.0, 1.0], 1);
    assert_eq!(vector_hits.first().map(|hit| hit.doc_id), Some(2));
}

#[test]
fn sql_golden_fixture_passes_on_sqlite_key_value_storage() {
    let fixture = load_fixture();
    let dir = tempdir().unwrap();
    let path = dir.path().join("keyvalue-golden.sqlite3");
    let engine = open_key_value_engine(&path);

    for stmt in &fixture.schema_sql {
        engine.sql(stmt, &[]).expect("schema sql");
    }
    for stmt in &fixture.data_sql {
        engine.sql(stmt, &[]).expect("data sql");
    }
    for case in &fixture.cases {
        let result = engine
            .sql(&case.sql, &[])
            .unwrap_or_else(|err| panic!("[{}] sql error: {err}", case.name));
        assert_eq!(
            result.rows.len(),
            case.expected.len(),
            "[{}] row count differs",
            case.name
        );
        for (i, (got, expected)) in result.rows.iter().zip(case.expected.iter()).enumerate() {
            let expected = expected.as_object().unwrap_or_else(|| {
                panic!(
                    "[{}] expected row {i} is not an object: {expected}",
                    case.name
                )
            });
            for (column, expected_json) in expected {
                let expected = json_to_value(expected_json);
                let actual = got.get(column).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    actual, expected,
                    "[{} idx {i}] column {column:?}: got {:?}, expected {:?}",
                    case.name, actual, expected
                );
            }
        }
    }
}
