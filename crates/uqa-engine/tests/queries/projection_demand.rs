//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected function identities and consumer demand against independent `PostgreSQL` observations.

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
fn function_star_identity_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("function-star.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/function_star_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"].as_array_mut().unwrap().retain(|case| {
        !matches!(
            case["id"].as_str().unwrap(),
            "version" | "schema" | "routine" | "one_argument_routine"
        )
    });
    assert_eq!(restored["cases"].as_array().unwrap().len(), 24);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn union_output_demand_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("union-demand.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/union_output_demand_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn join_output_demand_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("join-demand.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/join_output_demand_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn original_output_demand_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(
        provider,
        &directory.path().join("original-output-demand.db"),
    );
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/projection_output_oracle.expected.json"),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn cte_definition_scopes_match_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cte-definition-scopes.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/cte_definition_scope_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "view_read"
                | "routine_read"
                | "catalog_same_name"
                | "different_types"
                | "column_aliases"
                | "quoted_names"
                | "definition_chain"
                | "repeated_not_materialized"
                | "recursive_self"
                | "recursive_forward"
                | "unused_enclosing_output"
        )
    });
    assert_eq!(restored["cases"].as_array().unwrap().len(), 11);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}
