//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::Value;
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
fn temporary_materialized_lifecycle_matches_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("temporary-materialized.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/temporary_materialized_views_oracle.expected.json"
    ))
    .unwrap();
    let mut initial = reference.clone();
    initial["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] != true);
    assert_eq!(initial["cases"].as_array().unwrap().len(), 34);
    crate::pg18_oracle::verify(&engine, &initial.to_string());
    if provider == 0 {
        return;
    }
    let peer = engine.new_session().unwrap();
    let missing = peer
        .sql("SELECT n FROM pg_temp.renamed_materialized", &[])
        .unwrap_err();
    assert_eq!(missing.sqlstate(), Some("42P01"));
    peer.sql(
        "CREATE MATERIALIZED VIEW pg_temp.renamed_materialized AS SELECT 8 AS n",
        &[],
    )
    .unwrap();
    assert_eq!(
        engine
            .sql("SELECT n FROM pg_temp.renamed_materialized", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(3)
    );
    assert_eq!(
        peer.sql("SELECT n FROM pg_temp.renamed_materialized", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(8)
    );
    drop(engine);
    assert_eq!(
        peer.sql("SELECT n FROM pg_temp.renamed_materialized", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(8)
    );
    drop(peer);
    let reopened = open(provider, &path);
    let mut reference = reference;
    reference["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    assert_eq!(reference["cases"].as_array().unwrap().len(), 3);
    crate::pg18_oracle::verify(&reopened, &reference.to_string());
}
