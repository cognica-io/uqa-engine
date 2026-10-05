//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Nested command results independently checked against `PostgreSQL` 18.4 in Docker.

use std::{path::Path, sync::Arc};

use rstest::rstest;
use uqa_core::Value;
use uqa_engine::Engine;

fn open(path: &Path, backend: &str) -> Engine {
    match backend {
        "memory" => Engine::new(),
        "native" => Engine::open(path).unwrap(),
        "kv" => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        "redb" => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest]
fn indexed_nested_commands_find_staged_insert_rows(
    #[values("memory", "native", "kv", "redb")] backend: &str,
    #[values("UPDATE t SET v = v + 1000", "DELETE FROM t")] command: &str,
    #[values("v = k", "v BETWEEN k AND k", "v IS NULL")] predicate: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(&directory.path().join("nested.db"), backend);
    engine
        .sql(
            "CREATE TABLE t (id integer PRIMARY KEY, v integer); CREATE INDEX t_v ON t(v)",
            &[],
        )
        .unwrap();
    engine.sql(&format!("CREATE FUNCTION mark(k integer) RETURNS integer VOLATILE LANGUAGE sql AS $$ {command} WHERE {predicate} RETURNING id $$"), &[]).unwrap();
    let first = if predicate == "v IS NULL" {
        "NULL"
    } else {
        "g"
    };
    let result = engine.sql(&format!("INSERT INTO t SELECT g, CASE WHEN g = 2 THEN coalesce(mark(1), 0) ELSE {first} END FROM generate_series(1, 2) AS g RETURNING id, v"), &[]).unwrap();
    assert_eq!(result.affected_rows, 2);
    assert_eq!(
        result.rows[1]["v"],
        Value::Int(1),
        "{backend}: {command} WHERE {predicate}"
    );
}
