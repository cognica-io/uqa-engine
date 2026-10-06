//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Finite extraction preserves exact values, selected overloads and retained session semantics.

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
fn finite_extraction_matches_postgresql_and_survives_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("finite-extraction.db");
    let engine = open(provider, &path);
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/finite_extraction_oracle.expected.json"
    ))
    .unwrap();
    let default_after_reopen = reference["cases"].as_array_mut().unwrap().pop().unwrap();
    assert_eq!(default_after_reopen["id"], "reopen_default");
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open(provider, &path)
    };
    if provider != 0 {
        reference["cases"].as_array_mut().unwrap().retain(|case| {
            matches!(
                case["id"].as_str().unwrap(),
                "catalog"
                    | "language_rows"
                    | "language_columns"
                    | "language_relation"
                    | "language_description"
                    | "date_wrapper"
                    | "information_owner"
                    | "retained_definitions"
                    | "retained_reopen"
            )
        });
        assert_eq!(reference["cases"].as_array().unwrap().len(), 9);
        crate::pg18_oracle::verify(&engine, &reference.to_string());
    }
    reference["cases"] = serde_json::json!([default_after_reopen]);
    crate::pg18_oracle::verify(&engine, &reference.to_string());
}
