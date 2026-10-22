//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Outer-row VALUES across native providers, routine calls and durable views.

use uqa_engine::Engine;

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
fn lateral_values_match_postgresql_in_live_and_reopened_scopes(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lateral-values.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/lateral_values_oracle.expected.json");
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
            "view_rows" | "routine_rows" | "routine_lateral" | "rollback_rows"
        )
    });
    assert_eq!(restored["cases"].as_array().unwrap().len(), 4);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}
