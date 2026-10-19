//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cross-type datetime operators retain session timezone and SQL semantics through physical consumers and durable reopen.

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
fn date_timestamp_consumers_match_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("date-timestamp.db");
    let engine = open(provider, &path);
    let transcript = include_str!(
        "../../../../tests/parity/pg18/date_timestamp_comparison_oracle.expected.json"
    );
    crate::pg18_oracle::verify(&engine, transcript);
    if provider != 0 {
        drop(engine);
        let engine = open(provider, &path);
        let mut reference: serde_json::Value = serde_json::from_str(transcript).unwrap();
        reference["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| case["reopen"].as_bool() == Some(true));
        assert_eq!(reference["cases"].as_array().unwrap().len(), 118);
        crate::pg18_oracle::verify(&engine, &reference.to_string());
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn temporal_retrieval_filters_match_postgresql_and_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("temporal-retrieval.db");
    let engine = open(provider, &path);
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/temporal_retrieval_oracle.expected.json"
    ))
    .unwrap();
    engine
        .sql(reference["setup"].as_str().unwrap(), &[])
        .unwrap();
    engine
        .sql(
            "CREATE INDEX datetime_retrieval_body ON datetime_retrieval USING gin(body)",
            &[],
        )
        .unwrap();
    // Every body is one token, so the PG reference's exact text condition and the UQA search leaf have the same document support.
    for case in reference["cases"].as_array_mut().unwrap() {
        case["sql"] = case["sql"]
            .as_str()
            .unwrap()
            .replace("body = 'database'", "text_match(body, 'database')")
            .into();
    }
    let transcript = reference.to_string();
    crate::pg18_oracle::verify(&engine, &transcript);
    if provider != 0 {
        drop(engine);
        crate::pg18_oracle::verify(&open(provider, &path), &transcript);
    }
}
