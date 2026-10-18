//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_engine::Engine;

#[path = "collation_case_mapping/legacy.rs"]
mod legacy;

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
fn collation_case_mapping_matches_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("collation-case-mapping.db");
    let engine = open(provider, &path);
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/collation_case_mapping_oracle.expected.json"
    )))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    if provider == 0 {
        return;
    }
    drop(engine);
    // Stored views, CHECK constraints, defaults, generated columns, triggers, rules, routines and unique expression indexes keep each call's input collation after reopening.
    let engine = open(provider, &path);
    reference["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "columns_upper"
                | "columns_initcap"
                | "columns_casefold"
                | "columns_ilike"
                | "column_override"
                | "conflict_lower"
                | "conflict_ilike"
                | "nested_calls"
                | "collation_for"
                | "view_rows"
                | "checked_rejects_x"
                | "checked_rows"
                | "checked_reopen_probe"
                | "defaults_after_update"
                | "defaults_reopen_probe"
                | "trigger_rows"
                | "trigger_reopen_probe"
                | "unique_rows"
                | "unique_reopen_probe"
                | "rule_rows"
                | "rule_reopen_probe"
                | "call_sql_function"
                | "call_plpgsql_function"
                | "domain_collation"
                | "name_collation"
                | "updated_row"
                | "merged_row"
        )
    });
    assert_eq!(reference["cases"].as_array().unwrap().len(), 27);
    crate::pg18_oracle::verify(&engine, &reference.to_string());
}
