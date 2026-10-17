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
fn set_returning_contexts_match_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("set-returning-contexts.db");
    let engine = open(provider, &path);
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/set_returning_contexts_oracle.expected.json"
    )))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    if provider == 0 {
        return;
    }
    drop(engine);
    // The rows each expanded INSERT stored stay as written, stored views keep expanding their set-returning calls in window keys and deferring the functions in FROM, a stored rule still evaluates a set-returning VALUES entry outside a target list, and stored routines keep their placement and column definition list diagnostics and their FROM column names.
    let engine = open(provider, &path);
    reference["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "update_set_user"
                | "call_argument"
                | "call_argument_user"
                | "call_argument_valid"
                | "fixtures_unchanged"
                | "window_view_rows"
                | "insert_regexp_rows"
                | "insert_m_rows"
                | "insert_k_rows"
                | "insert_n_rows"
                | "insert_partitioned_rows"
                | "insert_rule_reads_set"
                | "insert_rule_rows"
                | "coldef_user_scalar"
                | "coldef_record_required"
                | "coldef_record"
                | "coldef_out_parameters"
                | "coldef_composite"
                | "coldef_composite_out"
                | "coldef_composite_domain"
                | "lazy_view_unfetched"
                | "lazy_view_fetched"
                | "lazy_user_rows_from"
                | "lazy_user_view_unfetched"
                | "lazy_user_view_fetched"
                | "user_alias_reference"
                | "user_rows_from_columns"
        )
    });
    assert_eq!(reference["cases"].as_array().unwrap().len(), 27);
    crate::pg18_oracle::verify(&engine, &reference.to_string());
}
