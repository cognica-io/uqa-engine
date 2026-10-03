//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL index creation and mutation keep the provider's bounded HNSW publication path.

use std::{path::Path, sync::Arc};
use uqa_core::Value;
use uqa_engine::{Engine, SQLParam};
use uqa_storage::mvcc::VersionedSessionOptions;
use uqa_storage_sqlite::{ManagedConnection, SQLiteKeyValueStorage, SQLiteStorageProvider};

const DIMENSIONS: usize = 1024;
const DOCUMENTS: usize = 80;
const ALLOWANCE: usize = 256 * 1024;

fn open(kind: &str, path: &Path) -> Engine {
    let options = VersionedSessionOptions {
        retained_bytes: ALLOWANCE,
    };
    match kind {
        "memory" => Engine::new(),
        "sqlite" => {
            let connection = ManagedConnection::open(path).unwrap();
            connection.bind_native_records(options).unwrap();
            Engine::from_persistent_provider(Arc::new(SQLiteStorageProvider::new(connection)))
                .unwrap()
        }
        "key_value" => Engine::from_persistent_provider(Arc::new(
            SQLiteKeyValueStorage::open_with_options(path, options).unwrap(),
        ))
        .unwrap(),
        "redb" => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open_with_options(path, options).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

fn vector(document: usize) -> SQLParam {
    let mut values = vec![0.0; DIMENSIONS];
    values[document] = 1.0;
    values[0] = 0.125;
    SQLParam::vector(values)
}

fn nearest(engine: &Engine, document: usize) -> Value {
    let rows = engine
        .sql(
            "SELECT id FROM items WHERE knn_match(embedding, $1, 1) ORDER BY _score DESC, id",
            &[vector(document)],
        )
        .unwrap();
    rows.rows[0].get("id").unwrap().clone()
}

fn lifecycle(kind: &str) {
    assert!(DOCUMENTS * DIMENSIONS * size_of::<f32>() > ALLOWANCE);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hnsw.db");
    let engine = open(kind, &path);
    engine.sql("SET work_mem = '64kB'", &[]).unwrap();
    engine
        .sql(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, embedding VECTOR(1024))",
            &[],
        )
        .unwrap();
    for document in 1..=DOCUMENTS {
        engine
            .sql(
                "INSERT INTO items VALUES ($1, $2)",
                &[
                    SQLParam::scalar(Value::Int(document as i64)),
                    vector(document),
                ],
            )
            .unwrap();
    }
    engine.sql(
        "CREATE INDEX items_hnsw ON items USING hnsw (embedding) WITH (m = 2, ef_construction = 4, ef_search = 128, rebuild_threshold = 3)",
        &[],
    ).unwrap();
    assert_eq!(nearest(&engine, 17), Value::Int(17), "{kind}");
    engine.sql("BEGIN; SAVEPOINT original", &[]).unwrap();
    engine
        .sql(
            "UPDATE items SET embedding = $1 WHERE id = 17",
            &[vector(250)],
        )
        .unwrap();
    engine
        .sql("ROLLBACK TO SAVEPOINT original; COMMIT", &[])
        .unwrap();
    assert_eq!(nearest(&engine, 17), Value::Int(17), "{kind}");
    engine
        .sql(
            "UPDATE items SET embedding = $1 WHERE id = 17",
            &[vector(250)],
        )
        .unwrap();
    engine
        .sql("DELETE FROM items WHERE id IN (8, 9)", &[])
        .unwrap();
    assert_eq!(nearest(&engine, 250), Value::Int(17), "{kind}");
    drop(engine);
    if kind != "memory" {
        let reopened = open(kind, &path);
        assert_eq!(nearest(&reopened, 250), Value::Int(17), "{kind} reopened");
        let rows = reopened
            .sql("SELECT count(*) AS n FROM items", &[])
            .unwrap();
        assert_eq!(
            rows.rows[0].get("n"),
            Some(&Value::Int(DOCUMENTS as i64 - 2))
        );
    }
}

#[test]
fn memory_hnsw_sql_lifecycle() {
    lifecycle("memory");
}
#[test]
fn native_hnsw_sql_spill_lifecycle() {
    lifecycle("sqlite");
}
#[test]
fn key_value_hnsw_sql_spill_lifecycle() {
    lifecycle("key_value");
}
#[test]
fn redb_hnsw_sql_spill_lifecycle() {
    lifecycle("redb");
}
