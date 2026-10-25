//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete temporal carriers retain SQL behavior through indexes, rollback and reopen.

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
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn temporal_carriers_preserve_full_range_sql_and_durable_state(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("temporal-carriers.db");
    let engine = open(provider, &path);
    let transcript = include_str!("temporal_carriers.json");
    crate::pg18_oracle::verify(&engine, transcript);
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../uqa-sql/src/expr/time/construct/tests.json"),
    );
    if provider != 0 {
        drop(engine);
        let engine = open(provider, &path);
        let mut reference: serde_json::Value = serde_json::from_str(transcript).unwrap();
        reference["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| case["reopen"] == true);
        crate::pg18_oracle::verify(&engine, &reference.to_string());
    }
}
