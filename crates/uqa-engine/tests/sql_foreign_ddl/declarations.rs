//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign declaration order, stored constraints and failed publication against `PostgreSQL`.

use std::{path::Path, sync::Arc};
use tempfile::TempDir;
use uqa_engine::Engine;

const ORACLE: &str =
    include_str!("../../../../tests/parity/pg18/foreign_table_declaration_oracle.expected.json");

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
fn foreign_declarations_match_postgresql(#[case] provider: usize) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("foreign-declarations.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(&engine, ORACLE);
    if provider != 0 {
        drop(engine);
        let reopened = open(provider, &path);
        let oracle: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
        for expected in oracle["cases"].as_array().unwrap().iter().filter(|case| {
            case["id"]
                .as_str()
                .is_some_and(|identity| identity.starts_with("reopen."))
        }) {
            let sql = expected["sql"].as_str().unwrap();
            let actual = crate::pg18_oracle::run_case(&reopened, sql);
            assert_eq!(actual["error"], expected["error"], "reopened: {sql}");
            assert_eq!(actual["results"], expected["results"], "reopened: {sql}");
        }
    }
}
