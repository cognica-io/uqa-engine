//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` constraint output, namespace visibility and durable definitions.
//! Raw capture SHA-256: 04a230a2fb23313081608c155b88f4c34e363b03ab51e115c80b833f8e3db113.

use uqa_engine::Engine;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn constraint_definition_output_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("constraint-output.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(&engine, include_str!("constraint_output.json"));
    drop(engine);
    if provider != 0 {
        crate::pg18_oracle::verify(
            &open(provider, &path),
            include_str!("constraint_output_reopen.json"),
        );
    }
}

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
