//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` prepared input lifetime through DDL, rollback, planning and namespace changes.

use std::sync::Arc;
use uqa_engine::Engine;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn prepared_input_invalidation_matches_postgresql(
    #[case] provider: usize,
    #[values(0, 1, 2)] fixture: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("prepared-invalidation.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(
        &engine,
        match fixture {
            0 => include_str!(
                "../../../../tests/parity/pg18/prepared_input_invalidation_oracle.expected.json"
            ),
            1 => include_str!(
                "../../../../tests/parity/pg18/prepared_domain_invalidation_oracle.expected.json"
            ),
            2 => {
                include_str!("../../../../tests/parity/pg18/prepared_catalog_inputs.expected.json")
            }
            _ => unreachable!(),
        },
    );
}

fn open(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn prepared_inputs_follow_only_committed_peer_catalog_dependencies(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("prepared-peer.db"));
    crate::pg18_oracle::verify_peer(
        &engine,
        include_str!("../../../../tests/parity/pg18/prepared_dependency_sessions.expected.json"),
    );
}
