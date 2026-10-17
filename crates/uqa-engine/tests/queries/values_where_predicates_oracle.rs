//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` predicates over VALUES lists, lateral and function sources, and unaliased relations in every query position.

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
fn values_where_predicates_match_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("values-where-predicates.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/values_where_predicates_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    // Stored views and SQL routine bodies keep the predicates on their VALUES, function, unaliased and join-alias sources and their whole-row references across reopening, and the modified rows persist.
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "values_view_rows"
                | "lateral_view_rows"
                | "unaliased_view_rows"
                | "alias_view_rows"
                | "whole_row_view_rows"
                | "routine_call"
                | "table_rows"
                | "target_rows"
        )
    });
    assert_eq!(restored["cases"].as_array().unwrap().len(), 8);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}
