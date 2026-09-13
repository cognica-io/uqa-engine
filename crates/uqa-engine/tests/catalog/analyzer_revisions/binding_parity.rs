//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The same persistent SQL contract consumed by Python, Node.js, and WASM.

use super::{Backend, TempDir};
use serde_json::{json, Value as Json};
use uqa_core::Value;
use uqa_engine::SQLParam;

fn json_value(value: &Value) -> Json {
    match value {
        Value::Int(value) => json!(value),
        Value::Str(value) => json!(value),
        Value::JsonB(value) => serde_json::from_str(value).unwrap(),
        other => panic!("unexpected binding fixture value: {other:?}"),
    }
}

#[test]
fn nori_binding_contract_preserves_resources_graphs_and_reopen() {
    verify_contract(false);
}

#[test]
fn nori_binding_contract_survives_backup_restore_without_the_original_database() {
    verify_contract(true);
}

fn verify_contract(restore_backup: bool) {
    let fixture: Json = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/nori/bindings.json"
    )))
    .unwrap();
    assert_eq!(fixture["schema_version"], 1);
    let enabled = uqa_analysis::get_analyzer("nori").is_ok();
    assert!(
        !cfg!(feature = "nori") || enabled,
        "an Engine built with Nori must register the bundled analyzer"
    );
    let mode = if enabled { "enabled" } else { "disabled" };
    for backend in [Backend::SQLite, Backend::Redb] {
        let mut directory = TempDir::new().unwrap();
        let mut database = directory.path().join("nori-bindings.db");
        let mut engine = Some(backend.open(&database));
        for step in fixture[mode].as_array().unwrap() {
            let context = format!(
                "{backend:?}/{mode}/backup={restore_backup}/{}",
                step["name"]
            );
            if step["reopen"].as_bool() == Some(true) {
                drop(engine.take());
                if restore_backup {
                    let restored_directory = TempDir::new().unwrap();
                    let restored_database = restored_directory.path().join("restored.db");
                    std::fs::copy(&database, &restored_database).unwrap();
                    directory.close().unwrap();
                    assert!(!database.exists(), "{context}: original database remains");
                    directory = restored_directory;
                    database = restored_database;
                }
                engine = Some(backend.open(&database));
                continue;
            }
            let params: Vec<_> = step["params"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|value| SQLParam::Scalar(Value::Str(value.as_str().unwrap().into())))
                .collect();
            let result = engine
                .as_ref()
                .unwrap()
                .sql(step["sql"].as_str().unwrap(), &params);
            if let Some(message) = step["error_contains"].as_str() {
                let error = result.expect_err(&context).to_string();
                assert!(error.contains(message), "{context}: {error}");
            } else {
                let result = result.unwrap_or_else(|error| panic!("{context}: {error}"));
                let expected = step["rows_ref"]
                    .as_str()
                    .map(|key| &fixture[key])
                    .or_else(|| step.get("rows"));
                if let Some(expected) = expected {
                    let rows: Vec<Json> = result
                        .rows
                        .iter()
                        .map(|row| {
                            Json::Object(
                                row.iter()
                                    .map(|(key, value)| (key.clone(), json_value(value)))
                                    .collect(),
                            )
                        })
                        .collect();
                    assert_eq!(Json::Array(rows), *expected, "{context}");
                }
            }
        }
    }
}
