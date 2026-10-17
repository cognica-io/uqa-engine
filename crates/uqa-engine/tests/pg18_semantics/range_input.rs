//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range input diagnostics and canonical values across durable provider boundaries.

use uqa_engine::Engine;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn multirange_constructor_inputs_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("multirange-input.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../uqa-sql/src/expr/scalar_range/tests/constructor-inputs-postgresql.json"
        )),
    );
}

pub(super) fn open(provider: usize, path: &std::path::Path) -> Engine {
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
fn range_input_matches_postgresql_and_durable_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("range-input.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/range_input_oracle.expected.json"
    ))
    .unwrap();
    let mut initial = reference.clone();
    initial["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] != true);
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
        .retain(|case| case["reopen"] == true);
    assert_eq!(reopened["cases"].as_array().unwrap().len(), 2);
    crate::pg18_oracle::verify(&engine, &reopened.to_string());
}
