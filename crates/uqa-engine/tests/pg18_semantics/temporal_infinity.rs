//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Infinite temporal values through SQL execution, indexes and durable providers.

use uqa_engine::Engine;

fn open(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
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
fn temporal_infinity_matches_postgresql_execution_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("temporal-infinity.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/temporal_infinity_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../uqa-execution/src/aggregation/tests/interval.json"),
    );
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../uqa-sql/src/expr/scalar_range/tests/empty.json"),
    );
    if provider != 0 {
        drop(engine);
        let engine = open(provider, &path);
        let mut reference: serde_json::Value = serde_json::from_str(transcript).unwrap();
        reference["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| case["reopen"].as_bool() == Some(true));
        assert_eq!(reference["cases"].as_array().unwrap().len(), 47);
        crate::pg18_oracle::verify(&engine, &reference.to_string());
    }
}
