//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Finite `date_trunc` units, interval fields and stored temporal expression lifetimes.

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
fn date_trunc_interval_matches_postgresql_and_survives_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("date-trunc-interval.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/date_trunc_interval_oracle.expected.json");
    let mut reference: serde_json::Value = serde_json::from_str(transcript).unwrap();
    let default_after_reopen = reference["cases"].as_array_mut().unwrap().pop().unwrap();
    assert_eq!(default_after_reopen["id"], "reopen_default");
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open(provider, &path)
    };
    if provider != 0 {
        reference["cases"].as_array_mut().unwrap().retain(|case| {
            matches!(
                case["id"].as_str().unwrap(),
                "interval_limits" | "boundary_inputs" | "stored_definitions" | "retained_reopen"
            )
        });
        assert_eq!(reference["cases"].as_array().unwrap().len(), 4);
        crate::pg18_oracle::verify(&engine, &reference.to_string());
    }
    reference["cases"] = serde_json::json!([default_after_reopen]);
    crate::pg18_oracle::verify(&engine, &reference.to_string());
}
