//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected series signatures in retained SQL bodies and views.

use std::{path::Path, sync::Arc};
use uqa_engine::Engine;

const ORACLE: &str =
    include_str!("../../../../tests/parity/pg18/series_binding_oracle.expected.json");

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

fn verify_reopened(engine: &Engine) {
    let mut oracle: serde_json::Value = serde_json::from_str(ORACLE).unwrap();
    oracle["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["id"] == "reopen_values");
    assert_eq!(oracle["cases"].as_array().unwrap().len(), 1);
    crate::pg18_oracle::verify(engine, &oracle.to_string());
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn series_bindings_match_postgresql_and_survive_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("series-bindings.db");
    let engine = open(provider, &path);
    crate::pg18_oracle::verify(&engine, ORACLE);
    if provider != 0 {
        drop(engine);
        verify_reopened(&open(provider, &path));
    }
}

fn downgrade_series_binding(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(values) => values.iter_mut().map(downgrade_series_binding).sum(),
        serde_json::Value::Object(fields) => {
            let mut changed = 0;
            if fields.get("builtin") == Some(&serde_json::Value::Bool(true))
                && fields.get("name").and_then(serde_json::Value::as_str)
                    == Some("pg_catalog.generate_series")
            {
                if let Some(arguments) = fields
                    .get_mut("argument_types")
                    .and_then(serde_json::Value::as_array_mut)
                {
                    if arguments.len() == 2 {
                        arguments.push(arguments[0].clone());
                        changed += 1;
                    }
                }
            }
            changed
                + fields
                    .values_mut()
                    .map(downgrade_series_binding)
                    .sum::<usize>()
        }
        _ => 0,
    }
}

#[test]
fn legacy_series_definitions_migrate_before_binding_and_persist() {
    use uqa_storage_sqlite::ManagedConnection;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy-series.db");
    {
        let engine = Engine::open(&path).unwrap();
        crate::pg18_oracle::verify(&engine, ORACLE);
    }
    {
        let catalog =
            crate::native_storage::catalog(ManagedConnection::open(&path).unwrap()).unwrap();
        let mut definitions: serde_json::Value =
            serde_json::from_str(&catalog.get_metadata("sql_functions_json").unwrap().unwrap())
                .unwrap();
        assert_eq!(downgrade_series_binding(&mut definitions), 1);
        catalog
            .set_metadata("sql_functions_json", &definitions.to_string())
            .unwrap();
        let mut changed = 0;
        for mut view in catalog.load_views().unwrap() {
            let mut definition: serde_json::Value =
                serde_json::from_str(&view.definition_json).unwrap();
            changed += downgrade_series_binding(&mut definition);
            view.definition_json = definition.to_string();
            catalog.save_view(&view).unwrap();
        }
        assert_eq!(changed, 3);
    }
    for _ in 0..2 {
        verify_reopened(&Engine::open(&path).unwrap());
    }
    let catalog = crate::native_storage::catalog(ManagedConnection::open(&path).unwrap()).unwrap();
    let definitions: serde_json::Value =
        serde_json::from_str(&catalog.get_metadata("sql_functions_json").unwrap().unwrap())
            .unwrap();
    assert_eq!(assert_correct_bindings(&definitions), 2);
    let count = catalog
        .load_views()
        .unwrap()
        .iter()
        .map(|view| assert_correct_bindings(&serde_json::from_str(&view.definition_json).unwrap()))
        .sum::<usize>();
    assert_eq!(count, 3);
}

fn assert_correct_bindings(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(values) => values.iter().map(assert_correct_bindings).sum(),
        serde_json::Value::Object(fields) => {
            let mut count = 0;
            if let (Some(args), Some(binding)) = (fields.get("args"), fields.get("binding")) {
                if binding["builtin"] == true && binding["name"] == "pg_catalog.generate_series" {
                    assert_eq!(
                        args.as_array().unwrap().len(),
                        binding["argument_types"].as_array().unwrap().len(),
                    );
                    count += 1;
                }
            }
            count + fields.values().map(assert_correct_bindings).sum::<usize>()
        }
        _ => 0,
    }
}
