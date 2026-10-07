//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema-wide routine ACL changes preserve `PostgreSQL` declaration order and durable identities.

use std::{path::Path, sync::Arc};
use uqa_engine::Engine;

const ORACLE: &str = include_str!(
    "../../../../../tests/parity/pg18/user_routine_schema_grants_oracle.expected.json"
);

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

fn selected_reference(keep: impl Fn(&serde_json::Value) -> bool) -> String {
    let mut reference: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
    reference["cases"].as_array_mut().unwrap().retain(keep);
    assert_ne!(reference["cases"].as_array().unwrap().len(), 0);
    reference.to_string()
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn schema_routine_grants_match_postgresql_and_survive_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("routine-grants.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(&engine, &selected_reference(|case| case["id"] != "cleanup"));
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        let reopened = open(provider, &path);
        let reference =
            selected_reference(|case| case["id"].as_str().unwrap().starts_with("reopen_"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&reference).unwrap()["cases"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        crate::pg18_oracle::verify(&reopened, &reference);
        reopened
    };
    crate::pg18_oracle::verify(&engine, &selected_reference(|case| case["id"] == "cleanup"));
}
