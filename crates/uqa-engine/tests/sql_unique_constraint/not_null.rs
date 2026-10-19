//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Primary-key preparation preserves original NOT NULL requirements and ALTER diagnostic order.

use std::{path::Path, sync::Arc};
use uqa_engine::Engine;

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
fn primary_key_not_null_requirements_match_postgresql_after_reopen(#[case] provider: usize) {
    verify_after_reopen(
        provider,
        include_str!("../../../../tests/parity/pg18/primary_key_not_null_oracle.expected.json"),
        "alter_plain_savepoint",
        "not_null_primary_probe",
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn inherited_primary_key_not_null_matches_postgresql_after_reopen(#[case] provider: usize) {
    verify_after_reopen(
        provider,
        include_str!(
            "../../../../tests/parity/pg18/inherited_primary_key_not_null_oracle.expected.json"
        ),
        "persistent_catalog",
        "inherited_pk_probe",
    );
}

fn verify_after_reopen(provider: usize, expected: &str, split_id: &str, schema: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("primary-key-not-null.db");
    let engine = open(provider, &path);
    let mut reference: serde_json::Value = serde_json::from_str(expected).unwrap();
    let cases = reference["cases"].take().as_array().unwrap().clone();
    let split = cases
        .iter()
        .position(|case| case["id"] == split_id)
        .unwrap();
    reference["cases"] = serde_json::Value::from(cases[..split].to_vec());
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    engine.sql("COMMIT", &[]).unwrap();
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open(provider, &path)
    };
    engine
        .sql(
            &format!("BEGIN; SET LOCAL search_path={schema},pg_catalog"),
            &[],
        )
        .unwrap();
    reference["cases"] = serde_json::Value::from(cases[split..].to_vec());
    crate::pg18_oracle::verify(&engine, &reference.to_string());
}
