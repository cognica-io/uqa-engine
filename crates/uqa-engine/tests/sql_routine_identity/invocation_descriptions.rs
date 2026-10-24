//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent routine value, side-effect and identity acceptance across providers.

use uqa_engine::Engine;

fn selected(
    reference: &serde_json::Value,
    mut filter: impl FnMut(&serde_json::Value) -> bool,
) -> String {
    let mut reference = reference.clone();
    reference["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| filter(case));
    reference.to_string()
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn routine_invocation_descriptions_match_postgresql(#[case] provider: usize) {
    verify_provider(
        provider,
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../uqa-sql/src/assignment/routines/tests/pg18_routine_descriptions.json"
        )),
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn routine_sql_body_descriptions_match_postgresql(#[case] provider: usize) {
    verify_provider(
        provider,
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../uqa-sql/src/assignment/routines/tests/pg18_routine_body_descriptions.json"
        )),
    );
}

fn verify_provider(provider: usize, reference: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("routine-descriptions.db");
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
    let reference: serde_json::Value = serde_json::from_str(reference).unwrap();
    let engine = open();
    crate::pg18_oracle::verify(
        &engine,
        &selected(&reference, |case| case["final_cleanup"] != true),
    );
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        let engine = open();
        crate::pg18_oracle::verify(
            &engine,
            &selected(&reference, |case| case["reopen"] == true),
        );
        engine
    };
    crate::pg18_oracle::verify(
        &engine,
        &selected(&reference, |case| case["final_cleanup"] == true),
    );
}
