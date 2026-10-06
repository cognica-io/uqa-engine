//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` array input timing, domain constraints and retained input effects.

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
fn domain_array_inputs_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("domain-array-inputs.db");
    let mut engine = open(provider, &path);
    let mut prefix: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/domain_array_input_oracle.expected.json"
    ))
    .unwrap();
    let mut suffix = prefix.clone();
    suffix["cases"] = prefix["cases"].as_array_mut().unwrap().split_off(16).into();
    crate::pg18_oracle::verify(&engine, &prefix.to_string());
    if provider != 0 {
        drop(engine);
        engine = open(provider, &path);
        let mut restored = prefix.clone();
        restored["cases"].as_array_mut().unwrap().drain(..2);
        crate::pg18_oracle::verify(&engine, &restored.to_string());
    }
    // Keep PREPARE and EXECUTE on the same session, including the domain change.
    // The transcript preserves earlier successful rows in the message that fails.
    crate::pg18_oracle::verify(&engine, &suffix.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn stored_domain_array_inputs_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("stored-domain-array-inputs.db");
    let mut engine = open(provider, &path);
    let mut prefix: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/domain_array_schema_oracle.expected.json"
    ))
    .unwrap();
    let mut restored = prefix.clone();
    restored["cases"] = prefix["cases"].as_array_mut().unwrap().split_off(32).into();
    crate::pg18_oracle::verify(&engine, &prefix.to_string());
    if provider != 0 {
        drop(engine);
        engine = open(provider, &path);
        let mut failures = prefix.clone();
        failures["cases"].as_array_mut().unwrap().retain(|case| {
            matches!(
                case["id"].as_str().unwrap(),
                "scalar_domain_default_assignment" | "null_domain_default_assignment"
            )
        });
        assert_eq!(failures["cases"].as_array().unwrap().len(), 2);
        // The loaded defaults must still perform their outer scalar constraints.
        crate::pg18_oracle::verify(&engine, &failures.to_string());
    }
    // nextval observes durable state without masking extra input checks at open.
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}
