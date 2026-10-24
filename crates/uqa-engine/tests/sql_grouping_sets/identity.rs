//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

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
fn distinct_grouping_identity_matches_postgresql_before_folding_and_after_reopen(
    #[case] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("grouping.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-sql/src/binding/preparation/tests/pg18_grouping_identity.json"
    )))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    if provider != 0 {
        drop(engine);
        let engine = open(provider, &path);
        let mut reference = reference;
        reference["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| case["reopen"] == true);
        crate::pg18_oracle::verify(&engine, &reference.to_string());
    }
}
