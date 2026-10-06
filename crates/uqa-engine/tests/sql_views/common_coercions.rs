//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Common-type conversions, cast origin and durable view definitions.

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
fn common_coercions_match_postgresql_and_survive_reopen(#[case] provider: usize) {
    verify_coercions(
        provider,
        include_str!("../../../../tests/parity/pg18/common_coercions_oracle.expected.json"),
        &[
            "reopen_definitions",
            "reopen_values",
            "reopen_stored_syntax",
            "reopen_arbiter_inference",
        ],
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn operator_coercions_match_postgresql_and_survive_reopen(#[case] provider: usize) {
    verify_coercions(
        provider,
        include_str!("../../../../tests/parity/pg18/operator_coercions_oracle.expected.json"),
        &[
            "reopen_definitions",
            "reopen_values",
            "reopen_stored_syntax",
        ],
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn array_concatenation_matches_postgresql_and_survives_reopen(#[case] provider: usize) {
    verify_coercions(
        provider,
        include_str!("../../../../tests/parity/pg18/array_concatenation_oracle.expected.json"),
        &["reopen_definitions", "reopen_values"],
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn membership_comparisons_match_postgresql_and_survive_reopen(#[case] provider: usize) {
    verify_coercions(
        provider,
        include_str!("../../../../tests/parity/pg18/membership_comparisons_oracle.expected.json"),
        &[
            "reopen_definition",
            "reopen_values",
            "reopen_subqueries",
            "reopen_outer_membership",
            "reopen_syntax",
            "reopen_index",
        ],
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn correlated_view_columns_match_postgresql_and_survive_reopen(#[case] provider: usize) {
    verify_coercions(
        provider,
        include_str!("../../../../tests/parity/pg18/correlated_view_scope_oracle.expected.json"),
        &["after"],
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn array_element_inputs_match_postgresql_and_survive_reopen(#[case] provider: usize) {
    verify_coercions(
        provider,
        include_str!("../../../../tests/parity/pg18/array_element_input_oracle.expected.json"),
        &["reopen_stored"],
    );
}

fn verify_coercions(provider: usize, transcript: &str, replay_ids: &[&str]) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("common-coercions.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| replay_ids.contains(&case["id"].as_str().unwrap()));
    assert_eq!(
        restored["cases"].as_array().unwrap().len(),
        replay_ids.len()
    );
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}
