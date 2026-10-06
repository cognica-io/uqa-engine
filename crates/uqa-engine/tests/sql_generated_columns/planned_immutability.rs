//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` plans a copy for mutability while retaining original catalog expressions.

use super::*;

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
fn stored_expression_mutability_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("generated-volatility.db");
    let engine = open(provider, &path);
    let reference =
        include_str!("../../../../tests/parity/pg18/generated_volatility_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, reference);
    if provider != 0 {
        drop(engine);
        let engine = open(provider, &path);
        let mut reference: serde_json::Value = serde_json::from_str(reference).unwrap();
        reference["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| case["id"] == "reopen_final_values");
        assert_eq!(reference["cases"].as_array().unwrap().len(), 1);
        crate::pg18_oracle::verify(&engine, &reference.to_string());
    }
}
