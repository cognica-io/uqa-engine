//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `CREATE SERVER` + `CREATE FOREIGN TABLE` DDL plumbing.

use std::collections::BTreeMap;
use std::fs::File;
use std::sync::Arc;

use arrow_array::{Int64Array, RecordBatch, StringArray};
use arrow_ipc::writer::FileWriter;
use arrow_schema::{DataType, Field, Schema};
use uqa_core::Value;
use uqa_engine::Engine;

fn row(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

#[test]
fn create_server_then_create_foreign_table() {
    let eng = Engine::new();
    eng.sql(
        "CREATE SERVER s1 FOREIGN DATA WRAPPER duckdb_fdw \
         OPTIONS (database 'sample.db')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE FOREIGN TABLE remote_books (id INTEGER, title TEXT) \
         SERVER s1 OPTIONS (source 'books.parquet')",
        &[],
    )
    .unwrap();
    let server = eng
        .foreign_server("s1")
        .unwrap()
        .expect("server registered");
    assert_eq!(server.fdw_type, "duckdb_fdw");
    assert_eq!(server.options.get("database").unwrap(), "sample.db");
    let table = eng
        .foreign_table("remote_books")
        .unwrap()
        .expect("foreign table");
    assert_eq!(table.server_name, "s1");
    assert_eq!(table.options.get("source").unwrap(), "books.parquet");
    assert_eq!(table.columns.len(), 2);
}

#[test]
fn unsupported_fdw_type_rejected() {
    let eng = Engine::new();
    let err = eng
        .sql(
            "CREATE SERVER bad FOREIGN DATA WRAPPER mongo_fdw OPTIONS (host 'a')",
            &[],
        )
        .unwrap_err();
    assert!(format!("{err:?}").contains("Unsupported FDW type"));
}

#[test]
fn unknown_foreign_table_metadata_is_an_error() {
    let eng = Engine::new();
    let error = eng
        .foreign_table_columns("missing_table")
        .expect_err("an unknown foreign table must not look like a zero-column table");
    assert!(error.contains("does not exist"), "{error}");
}

#[test]
fn unloaded_memory_foreign_table_is_an_error() {
    let eng = Engine::new();
    eng.sql(
        "CREATE SERVER mem FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE FOREIGN TABLE unloaded_rows (id INTEGER) \
         SERVER mem OPTIONS (source 'memory')",
        &[],
    )
    .unwrap();

    let error = eng
        .sql("SELECT * FROM unloaded_rows", &[])
        .expect_err("unloaded memory data must not be reported as an empty relation");
    assert!(format!("{error:?}").contains("no loaded memory data"));
}

#[test]
fn select_from_memory_foreign_table() {
    let eng = Engine::new();
    eng.sql(
        "CREATE SERVER mem FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE FOREIGN TABLE remote_books (id INTEGER, title TEXT, year INTEGER) \
         SERVER mem OPTIONS (source 'memory')",
        &[],
    )
    .unwrap();
    eng.load_memory_foreign_table(
        "remote_books",
        vec![
            row(&[
                ("id", Value::Int(1)),
                ("title", Value::Str("Rust".into())),
                ("year", Value::Int(2024)),
            ]),
            row(&[
                ("id", Value::Int(2)),
                ("title", Value::Str("Python".into())),
                ("year", Value::Int(2023)),
            ]),
            row(&[
                ("id", Value::Int(3)),
                ("title", Value::Str("UQA".into())),
                ("year", Value::Int(2024)),
            ]),
        ],
    )
    .unwrap();

    let result = eng
        .sql(
            "SELECT title FROM remote_books WHERE year = 2024 ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(result.columns, vec!["title"]);
    assert_eq!(result.rows.len(), 2);
    assert_eq!(
        result.rows[0].get("title"),
        Some(&Value::Str("Rust".into()))
    );
    assert_eq!(result.rows[1].get("title"), Some(&Value::Str("UQA".into())));
}

#[test]
fn memory_foreign_scan_is_pull_based_under_tiny_work_mem() {
    let eng = Engine::new();
    eng.sql("SET work_mem TO '1B'", &[]).unwrap();
    eng.sql(
        "CREATE SERVER mem FOREIGN DATA WRAPPER memory_fdw OPTIONS (kind 'memory')",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE FOREIGN TABLE remote_numbers (id INTEGER, parity INTEGER) \
         SERVER mem OPTIONS (source 'memory')",
        &[],
    )
    .unwrap();
    eng.load_memory_foreign_table(
        "remote_numbers",
        (0..4096_i64)
            .map(|id| row(&[("id", Value::Int(id)), ("parity", Value::Int(id % 2))]))
            .collect(),
    )
    .unwrap();

    let result = eng
        .sql(
            "SELECT count(*) AS total, max(id) AS maximum \
             FROM remote_numbers WHERE parity = 1",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["total"], Value::Int(2_048));
    assert_eq!(result.rows[0]["maximum"], Value::Int(4_095));
}

#[test]
fn select_from_duckdb_foreign_table() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("remote.duckdb");
    {
        let conn = duckdb::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE books (id INTEGER, title TEXT, year INTEGER);
             INSERT INTO books VALUES
               (1, 'Rust', 2024),
               (2, 'Python', 2023),
               (3, 'UQA', 2024);",
        )
        .unwrap();
    }

    let eng = Engine::new();
    let db_path = db_path.to_string_lossy().replace('\'', "''");
    eng.sql(
        &format!(
            "CREATE SERVER duck FOREIGN DATA WRAPPER duckdb_fdw OPTIONS (database '{db_path}')"
        ),
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE FOREIGN TABLE remote_books (id INTEGER, title TEXT, year INTEGER) \
         SERVER duck OPTIONS (source 'books')",
        &[],
    )
    .unwrap();

    let result = eng
        .sql(
            "SELECT title FROM remote_books WHERE year = 2024 ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(result.columns, vec!["title"]);
    assert_eq!(result.rows.len(), 2);
    assert_eq!(
        result.rows[0].get("title"),
        Some(&Value::Str("Rust".into()))
    );
    assert_eq!(result.rows[1].get("title"), Some(&Value::Str("UQA".into())));
}

#[test]
fn select_from_arrow_foreign_table() {
    let dir = tempfile::tempdir().unwrap();
    let arrow_path = dir.path().join("remote.arrow");
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("title", DataType::Utf8, false),
        Field::new("year", DataType::Int64, false),
    ]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(Int64Array::from(vec![1, 2, 3])),
            Arc::new(StringArray::from(vec!["Rust", "Python", "UQA"])),
            Arc::new(Int64Array::from(vec![2024, 2023, 2024])),
        ],
    )
    .unwrap();
    {
        let file = File::create(&arrow_path).unwrap();
        let mut writer = FileWriter::try_new(file, &schema).unwrap();
        writer.write(&batch).unwrap();
        writer.finish().unwrap();
    }

    let eng = Engine::new();
    eng.sql(
        "CREATE SERVER arrow_srv FOREIGN DATA WRAPPER arrow_fdw OPTIONS (kind 'ipc')",
        &[],
    )
    .unwrap();
    let source = arrow_path.to_string_lossy().replace('\'', "''");
    eng.sql(
        &format!(
            "CREATE FOREIGN TABLE remote_books (id INTEGER, title TEXT, year INTEGER) \
             SERVER arrow_srv OPTIONS (source '{source}')"
        ),
        &[],
    )
    .unwrap();

    let result = eng
        .sql(
            "SELECT title FROM remote_books WHERE year = 2024 ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(result.columns, vec!["title"]);
    assert_eq!(result.rows.len(), 2);
    assert_eq!(
        result.rows[0].get("title"),
        Some(&Value::Str("Rust".into()))
    );
    assert_eq!(result.rows[1].get("title"), Some(&Value::Str("UQA".into())));
}
