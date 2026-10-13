//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` constraint nodes, catalog reconstruction and transaction observations.

use uqa_engine::Engine;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn constraint_information_views_match_postgresql(#[case] provider: usize) {
    verify(provider, include_str!("constraint_information.json"));
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn constraint_key_visibility_matches_postgresql(#[case] provider: usize) {
    verify(provider, include_str!("constraint_key_visibility.json"));
}

fn verify(provider: usize, transcript: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("constraint-expressions.db");
    let engine = match provider {
        0 => Engine::new(),
        1 => Engine::open(&path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    crate::pg18_oracle::verify(&engine, transcript);
}
