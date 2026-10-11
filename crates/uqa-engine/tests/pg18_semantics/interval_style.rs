//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `IntervalStyle` through sessions, prepared input, routines, cursors and durable providers.

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
fn interval_style_matches_postgresql_execution_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("interval-style.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/interval_style_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    // Host-side rendering occurs after the statement scope has restored its caller.
    let result = engine
        .sql("SELECT interval '-1 days 1 hour' AS v", &[])
        .unwrap();
    engine.sql("SET IntervalStyle='iso_8601'", &[]).unwrap();
    let value = &result.rows[0]["v"];
    assert_eq!(
        uqa_engine::sql::format_postgres_text(value, &uqa_sql::ColumnType::Interval, Some(&engine))
            .unwrap(),
        "P-1DT1H"
    );
    if provider != 0 {
        drop(engine);
        let engine = open(provider, &path);
        let mut reference: serde_json::Value = serde_json::from_str(transcript).unwrap();
        reference["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| case["reopen"].as_bool() == Some(true));
        assert_eq!(reference["cases"].as_array().unwrap().len(), 26);
        crate::pg18_oracle::verify(&engine, &reference.to_string());
    }
}
