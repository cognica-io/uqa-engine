//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Replay `PostgreSQL`'s ordered column type analysis on empty and populated relations through every storage provider.

use std::{path::Path, sync::Arc};
use tempfile::TempDir;
use uqa_engine::Engine;

const ORACLE: &str =
    include_str!("../../../../../tests/parity/pg18/alter_type_analysis_oracle.expected.json");

fn open(provider: usize, path: &Path) -> Engine {
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
fn column_type_analysis_matches_postgresql(#[case] provider: usize) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("column-types.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(&engine, ORACLE);
    if provider != 0 {
        drop(engine);
        let reopened = open(provider, &path);
        let oracle: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
        let expected = oracle["cases"].as_array().unwrap().last().unwrap();
        let actual = crate::pg18_oracle::run_case(&reopened, expected["sql"].as_str().unwrap());
        assert_eq!(actual["error"], expected["error"]);
        assert_eq!(actual["results"], expected["results"]);
    }
}
