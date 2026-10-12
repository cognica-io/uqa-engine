//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;
use uqa_engine::Engine;

fn open(provider: usize, path: &std::path::Path) -> Engine {
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
fn boolean_tests_preserve_postgresql_analysis_effects_and_storage(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("boolean-tests.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!("expected.json")).unwrap();
    let mut initial = reference.clone();
    initial["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|c| c["reopen"] != true);
    crate::pg18_oracle::verify(&engine, &initial.to_string());
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open(provider, &path)
    };
    let mut reopened = reference;
    reopened["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|c| c["reopen"] == true);
    crate::pg18_oracle::verify(&engine, &reopened.to_string());
}
