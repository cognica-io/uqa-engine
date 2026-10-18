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
fn regexp_search_loop_matches_postgresql(#[case] provider: usize) {
    // The counting, position, substring and replacement functions search from their start position as PostgreSQL does on every storage provider; the transcript reads no stored state.
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("regexp-search-loop.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/parity/pg18/regexp_search_loop_oracle.expected.json"
        )),
    );
}
