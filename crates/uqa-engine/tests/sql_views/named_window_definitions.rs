//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected window identities, input ownership and durable definition output.

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
fn named_windows_match_postgresql_and_survive_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("named-windows.db");
    let engine = open(provider, &path);
    let transcript =
        include_str!("../../../../tests/parity/pg18/named_window_viewdef_oracle.expected.json");
    crate::pg18_oracle::verify(&engine, transcript);
    if provider == 0 {
        return;
    }
    drop(engine);
    let engine = open(provider, &path);
    let mut restored: serde_json::Value = serde_json::from_str(transcript).unwrap();
    restored["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "format_variants"
                | "result_values"
                | "shared_subquery_reused"
                | "retained_input_effects"
                | "catalog_definition"
                | "retained_definitions"
        )
    });
    assert_eq!(restored["cases"].as_array().unwrap().len(), 6);
    crate::pg18_oracle::verify(&engine, &restored.to_string());
}

#[test]
fn legacy_expanded_window_definitions_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-window-definitions.db");
    let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/named_window_viewdef_oracle.expected.json"
    ))
    .unwrap();
    fixture["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str().unwrap(),
            "setup" | "direct" | "nested_scopes" | "result_values"
        )
    });
    let engine = open(1, &path);
    crate::pg18_oracle::verify(&engine, &fixture.to_string());
    drop(engine);
    {
        let catalog = crate::native_storage::catalog(
            uqa_storage_sqlite::ManagedConnection::open(&path).unwrap(),
        )
        .unwrap();
        let views = catalog.load_views().unwrap();
        assert_eq!(views.len(), 2);
        for mut view in views {
            let mut definition: serde_json::Value =
                serde_json::from_str(&view.definition_json).unwrap();
            assert!(remove_window_identities(&mut definition) > 0);
            view.definition_json = definition.to_string();
            catalog.save_view(&view).unwrap();
        }
    }
    fixture["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["id"].as_str() == Some("result_values"));
    for _ in 0..2 {
        let engine = open(1, &path);
        crate::pg18_oracle::verify(&engine, &fixture.to_string());
        let definition = crate::pg18_oracle::run_case(
            &engine,
            "SELECT pg_get_viewdef('nw_direct'::regclass) AS definition",
        );
        let sql = definition["results"][0]["rows"][0][0].as_str().unwrap();
        assert!(sql.contains("OVER (PARTITION BY"), "{sql}");
        assert!(!sql.contains("WINDOW"), "{sql}");
    }
}

fn remove_window_identities(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(fields) => {
            fields.remove("windows");
            let removed = if fields.contains_key("partition_by") {
                usize::from(fields.remove("definition").is_some())
            } else {
                0
            };
            removed
                + fields
                    .values_mut()
                    .map(remove_window_identities)
                    .sum::<usize>()
        }
        serde_json::Value::Array(values) => values.iter_mut().map(remove_window_identities).sum(),
        _ => 0,
    }
}
