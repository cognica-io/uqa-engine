//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

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
fn builtin_namespace_relations_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("builtin-schemas.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/builtin_schema_storage_oracle.expected.json"
    ))
    .unwrap();
    let mut initial = reference.clone();
    initial["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] != true);
    assert_eq!(initial["cases"].as_array().unwrap().len(), 39);
    crate::pg18_oracle::verify(&engine, &initial.to_string());
    if provider == 0 {
        return;
    }
    let peer = engine.new_session().unwrap();
    let mut reads = reference.clone();
    reads["cases"].as_array_mut().unwrap().retain(|case| {
        case["reopen"] == true && !case["sql"].as_str().unwrap().contains("nextval")
    });
    crate::pg18_oracle::verify(&peer, &reads.to_string());
    drop(peer);
    drop(engine);
    let reopened = open(provider, &path);
    let mut reopened_reference = reference;
    reopened_reference["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    assert_eq!(reopened_reference["cases"].as_array().unwrap().len(), 9);
    crate::pg18_oracle::verify(&reopened, &reopened_reference.to_string());
}
