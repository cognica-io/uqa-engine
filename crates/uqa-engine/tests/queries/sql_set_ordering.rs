//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` set-operation ordering diagnostics through public queries and stored definitions.

use std::{path::Path, sync::Arc};
use uqa_engine::Engine;

fn open(provider: usize, path: &Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn set_operation_ordering_matches_postgresql_after_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("set-ordering.db");
    let engine = open(provider, &path);
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/set_operation_ordering_oracle.expected.json"
    ))
    .unwrap();
    let cases = reference["cases"].take().as_array().unwrap().clone();
    let split = cases
        .iter()
        .position(|case| case["id"] == "view_read")
        .unwrap();
    reference["cases"] = serde_json::Value::from(cases[..split].to_vec());
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open(provider, &path)
    };
    reference["cases"] = serde_json::Value::from(cases[split..].to_vec());
    crate::pg18_oracle::verify(&engine, &reference.to_string());
}
