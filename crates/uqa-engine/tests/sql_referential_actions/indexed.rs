//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent SQL outcomes for indexed referential actions and NULL parent keys.

use std::{path::Path, sync::Arc};
use uqa_engine::Engine;

fn open(provider: u8, path: &Path) -> Engine {
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
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn indexed_foreign_keys_match_postgresql(#[case] provider: u8) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("indexed-references.db");
    let engine = open(provider, &path);
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/fk_indexed_references_oracle.expected.json"
    ))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    drop(engine);
    if provider != 0 {
        let ids = reference["reopen_ids"].as_array().unwrap().clone();
        reference["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| ids.contains(&case["id"]));
        crate::pg18_oracle::verify(&open(provider, &path), &reference.to_string());
    }
}
