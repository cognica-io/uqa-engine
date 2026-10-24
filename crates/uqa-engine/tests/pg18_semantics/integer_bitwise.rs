//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Integer operator composition, prepared selection and retained definitions across providers.

use uqa_engine::Engine;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn integer_bitwise_compositions_match_postgresql_and_survive_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("integer-bitwise.db");
    let open = || match provider {
        0 => Engine::new(),
        1 => Engine::open(&path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/integer_bitwise_oracle.expected.json"
    ))
    .unwrap();
    let subset = |predicate: fn(&serde_json::Value) -> bool| {
        let mut selected = reference.clone();
        selected["cases"].as_array_mut().unwrap().retain(predicate);
        selected.to_string()
    };
    let engine = open();
    crate::pg18_oracle::verify(&engine, &subset(|case| case["after_reopen"] != true));
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        let engine = open();
        crate::pg18_oracle::verify(&engine, &subset(|case| case["reopen"] == true));
        engine
    };
    crate::pg18_oracle::verify(&engine, &subset(|case| case["after_reopen"] == true));
    for view in ["bitwise_view", "bitwise_prefix_view"] {
        let definition = crate::pg18_oracle::run_case(
            &engine,
            &format!("SELECT pg_get_viewdef('{view}'::regclass, true)"),
        );
        let sql = definition["results"][0]["rows"][0][0]
            .as_str()
            .expect("pretty view definition");
        engine
            .sql(&format!("CREATE VIEW {view}_roundtrip AS {sql}"), &[])
            .unwrap();
        for filter in ["WHERE id IN (1, 2, 5)", ""] {
            let original = crate::pg18_oracle::run_case(
                &engine,
                &format!("SELECT * FROM {view} {filter} ORDER BY id"),
            );
            let reparsed = crate::pg18_oracle::run_case(
                &engine,
                &format!("SELECT * FROM {view}_roundtrip {filter} ORDER BY id"),
            );
            if !filter.is_empty() {
                assert!(original["error"].is_null(), "{original}");
            }
            assert_eq!(reparsed, original, "{view}: {sql}");
        }
    }
}
