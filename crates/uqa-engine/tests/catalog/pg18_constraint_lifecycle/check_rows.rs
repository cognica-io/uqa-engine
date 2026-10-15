//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CHECK row scope, physical relation identity, dependencies and retained enforcement.
//!
//! `PostgreSQL` 18.4 reference: image bc8a94e4a938576533c0f0e8ad6f2e8c67d5c5facef05005a5e5cf9adce2fe25; raw runtime SHA-256 adea2e6891191fd1d6f959a9fbb3d7a53e853ff79b5411bd05f1d5b02d52a635 and quoted-name SHA-256 e853e42771cb44322f3df492ad7aec319f0dd7f179edadbd80d0124da30fbec9.

use tempfile::TempDir;
use uqa_engine::Engine;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn check_row_scope_and_enforcement_match_postgresql(#[case] provider: usize) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("check-row-runtime.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(&engine, include_str!("check_row_runtime.json"));
    drop(engine);
    if provider != 0 {
        crate::pg18_oracle::verify(
            &open(provider, &path),
            include_str!("check_row_runtime_reopen.json"),
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
