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
fn regular_expression_engine_matches_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("regular-expression-engine.db");
    let engine = open(provider, &path);
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/regular_expression_engine_oracle.expected.json"
    )))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    if provider == 0 {
        return;
    }
    drop(engine);
    // A stored view, CHECK constraints and a default keep the engine's matching and their input collations after reopening.
    let engine = open(provider, &path);
    reference["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "stored_view_rows"
                | "stored_check_rejects_c_class"
                | "stored_check_rejects_case"
                | "stored_check_rows"
                | "stored_default_values"
        )
    });
    assert_eq!(reference["cases"].as_array().unwrap().len(), 5);
    crate::pg18_oracle::verify(&engine, &reference.to_string());
}
