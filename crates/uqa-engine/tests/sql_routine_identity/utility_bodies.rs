//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine definition errors preserve PostgreSQL statement order and transaction effects.

use std::sync::Arc;
use uqa_engine::Engine;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn sql_standard_utility_bodies_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("routine-utilities.db");
    let engine = match provider {
        0 => Engine::new(),
        1 => Engine::open(&path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    crate::pg18_oracle::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/sql_standard_utilities_oracle.expected.json"),
    );
}
