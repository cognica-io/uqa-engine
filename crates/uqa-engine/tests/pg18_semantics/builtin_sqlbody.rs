//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored SQL reconstruction preserves selected routine identities and durable definitions.

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
fn builtin_sql_bodies_preserve_selected_names_after_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("builtin-sql-bodies.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/builtin_sqlbody_oracle.expected.json"
    ))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut reopened = reference.clone();
    reopened["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "builtin_bodies"
                | "null_bodies"
                | "user_bodies"
                | "stored_definitions"
                | "reopen_values"
        )
    });
    assert_eq!(reopened["cases"].as_array().unwrap().len(), 5);
    crate::pg18_oracle::verify(&engine, &reopened.to_string());
    engine
        .sql("SET search_path=body_shadow,pg_catalog,public", &[])
        .unwrap();
    let mut shadowed = reference;
    shadowed["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "shadowed_bodies"
                | "explicit_lookup"
                | "shadowed_user_bodies"
                | "shadowed_stored_definitions"
                | "shadowed_values"
        )
    });
    assert_eq!(shadowed["cases"].as_array().unwrap().len(), 5);
    crate::pg18_oracle::verify(&engine, &shadowed.to_string());
}
