//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{path::Path, sync::Arc};
use uqa_engine::Engine;

fn open(path: &Path, provider: u8) -> Engine {
    match provider {
        0 => Engine::open(path).unwrap(),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::memory(None)]
#[case::sqlite(Some(0))]
#[case::sqlite_key_value(Some(1))]
#[case::redb(Some(2))]
fn indexed_query_sources_match_postgresql(#[case] provider: Option<u8>) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("indexed.db");
    let engine = provider.map_or_else(Engine::new, |provider| open(&path, provider));
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/indexed_query_candidates_oracle.expected.json"
    ))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    drop(engine);
    if let Some(provider) = provider {
        let ids = reference["reopen_ids"].as_array().unwrap().clone();
        reference["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| ids.contains(&case["id"]));
        crate::pg18_oracle::verify(&open(&path, provider), &reference.to_string());
    }
}
