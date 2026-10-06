//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine declarations preserve stored bindings, attributes and source across reopening.

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
fn function_definitions_match_postgresql_and_survive_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("function-definitions.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/function_definition_oracle.expected.json"
    ))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open(provider, &path)
    };
    let mut reopened = reference;
    reopened["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "missing"
                | "helpers"
                | "support_metadata"
                | "catalog_types"
                | "builtin_internal"
                | "builtin_sql"
                | "aggregate"
                | "builtin_declarations"
                | "registered_builtin_definitions"
                | "read_only_after"
                | "renamed_definitions"
        )
    });
    assert_eq!(reopened["cases"].as_array().unwrap().len(), 11);
    crate::pg18_oracle::verify(&engine, &reopened.to_string());
    let definitions = engine.sql("SELECT pg_get_functiondef(p.oid) AS definition FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='def_test' ORDER BY proname", &[]).unwrap();
    assert_eq!(definitions.rows.len(), 17);
    for row in definitions.rows {
        let uqa_core::Value::Str(definition) = &row["definition"] else {
            panic!("routine definition is text");
        };
        engine
            .sql(definition, &[])
            .unwrap_or_else(|error| panic!("{definition}: {error}"));
    }
    reopened["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "renamed_definitions" | "read_only_after"
        )
    });
    crate::pg18_oracle::verify(&engine, &reopened.to_string());
    crate::pg18_oracle::verify(
        &engine,
        include_str!(
            "../../../../tests/parity/pg18/routine_return_replacement_oracle.expected.json"
        ),
    );
}
