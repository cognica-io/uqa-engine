//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL source-body input validation and session restoration against `PostgreSQL`.

use std::{path::Path, sync::Arc};
use uqa_engine::Engine;

const ORACLE: &str =
    include_str!("../../../../tests/parity/pg18/sql_body_inputs_oracle.expected.json");

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

fn selected(oracle: &serde_json::Value, filter: impl FnMut(&serde_json::Value) -> bool) -> String {
    let mut oracle = oracle.clone();
    oracle["cases"].as_array_mut().unwrap().retain(filter);
    oracle.to_string()
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn sql_body_inputs_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sql-body-inputs.db");
    let oracle: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(
        &engine,
        &selected(&oracle, |case| case["final_cleanup"] != true),
    );
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        let reopened = open(provider, &path);
        reopened.sql("SET search_path TO sb,public", &[]).unwrap();
        crate::pg18_oracle::verify(&reopened, &selected(&oracle, |case| case["reopen"] == true));
        reopened
    };
    engine.sql("RESET search_path", &[]).unwrap();
    crate::pg18_oracle::verify(
        &engine,
        &selected(&oracle, |case| case["final_cleanup"] == true),
    );
}
