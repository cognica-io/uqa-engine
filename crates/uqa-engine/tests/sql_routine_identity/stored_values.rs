//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL-standard VALUES bodies retain their dependency behavior across catalog changes and reopen.

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
fn stored_values_dependencies_match_postgresql_after_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("stored-values.db");
    let engine = open(provider, &path);
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/stored_values_dependencies_oracle.expected.json"
    ))
    .unwrap();
    let cases = reference["cases"].take().as_array().unwrap().clone();
    let split = cases
        .iter()
        .position(|case| case["id"] == "call_constant")
        .unwrap();
    reference["cases"] = serde_json::Value::from(cases[..split].to_vec());
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    engine.sql("COMMIT", &[]).unwrap();
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open(provider, &path)
    };
    engine
        .sql("SET search_path=stored_values_probe,pg_catalog", &[])
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    reference["cases"] = serde_json::Value::from(cases[split..].to_vec());
    crate::pg18_oracle::verify(&engine, &reference.to_string());
}
