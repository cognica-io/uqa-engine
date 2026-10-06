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
fn foreign_key_order_matches_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("foreign-key-order.db");
    let engine = open(provider, &path);
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/create_table_foreign_key_order_oracle.expected.json"
    ))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    reference["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "written_identity_order"
                | "multiple_catalog"
                | "automatic_catalog"
                | "alter_catalog"
                | "successful_catalog"
                | "shadow_sequence_dependency"
                | "identity_ownership"
                | "stored_rows"
                | "identity_rows"
                | "failed_relations"
                | "failed_constraints"
                | "shadow_explicit_rows"
                | "partition_identity_rows"
                | "shadow_serial_lookup"
                | "multiple_no_inherit"
        )
    });
    assert_eq!(reference["cases"].as_array().unwrap().len(), 15);
    crate::pg18_oracle::verify(&engine, &reference.to_string());
}
