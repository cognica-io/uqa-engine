//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Existing names do not suppress declaration errors or trigger a second index build.

use super::{definition, sessions, sql, Engine, Value};
use serde_json::json;
use uqa_sql::SQLError;

fn diagnostic(result: Result<uqa_sql::SQLResult, SQLError>) -> serde_json::Value {
    match result {
        Ok(_) => json!({"state": null}),
        Err(error) => {
            let (detail, hint) = match &error {
                SQLError::Diagnostic { detail, hint, .. } => (detail.as_deref(), hint.as_deref()),
                _ => (None, None),
            };
            json!({"state": error.sqlstate(), "message": error.to_string(), "detail": detail, "hint": hint})
        }
    }
}

#[test]
fn index_creation_validates_the_definition_before_an_existing_name() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE index_validation(id int, embedding vector(2)); INSERT INTO index_validation VALUES(1,ARRAY[1,0]),(1,ARRAY[0,1]); CREATE INDEX occupied_index ON index_validation(id)");
    let original = definition(&engine, "occupied_index");
    let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/index_definition_order.expected.json"
    )))
    .unwrap();
    let mut failures = Vec::new();
    for case in reference["cases"].as_array().unwrap() {
        sql(&engine, "BEGIN");
        let statement = case["sql"].as_str().unwrap();
        let actual = diagnostic(engine.sql(statement, &[]));
        sql(&engine, "ROLLBACK");
        if actual != case["expected"] {
            failures.push((case["label"].clone(), actual, case["expected"].clone()));
        }
        assert_eq!(definition(&engine, "occupied_index"), original);
        assert!(engine.catalog_index("new_index").unwrap().is_none());
    }
    assert!(
        failures.is_empty(),
        "PostgreSQL declaration differences: {failures:#?}"
    );
    assert_eq!(
        sql(&engine, "SELECT count(*) AS n FROM index_validation").rows[0]["n"],
        Value::Int(2)
    );
}

fn diskann_existing_name(engine: &Engine) {
    sql(engine, "CREATE TABLE diskann_validation(id int, embedding vector(2)); INSERT INTO diskann_validation VALUES(1,ARRAY[1,0]),(2,ARRAY[0,1]); CREATE INDEX occupied_diskann ON diskann_validation USING diskann(embedding)");
    let original = engine.catalog_index("occupied_diskann").unwrap().unwrap();
    let generation = selected_generation(engine);
    let search = "SELECT id,_score FROM diskann_validation WHERE knn_match(embedding,ARRAY[1,0],10) ORDER BY id";
    let expected = sql(engine, search).rows;
    sql(engine, "BEGIN; SAVEPOINT kept");
    sql(engine, "CREATE INDEX IF NOT EXISTS occupied_diskann ON diskann_validation USING diskann(embedding) WITH(max_degree=2,search_list_size=2,beam_width=1)");
    for optional in ["", "IF NOT EXISTS "] {
        for (suffix, state) in [
            ("(absent)", "42703"),
            ("(embedding) WITH(max_degree=0)", "42804"),
            ("(embedding) WITH(unknown_option=1)", "0A000"),
        ] {
            let error = engine.sql(&format!("CREATE INDEX {optional}occupied_diskann ON diskann_validation USING diskann{suffix}"), &[]).unwrap_err();
            assert_eq!(error.sqlstate(), Some(state), "{error}");
            sql(engine, "ROLLBACK TO kept");
        }
    }
    sql(engine, "COMMIT");
    assert_eq!(sql(engine, search).rows, expected);
    let current = engine.catalog_index("occupied_diskann").unwrap().unwrap();
    assert_eq!(current.definition_json, original.definition_json);
    assert_eq!(current.parameters_json, original.parameters_json);
    assert_eq!(selected_generation(engine), generation);
}

fn selected_generation(engine: &Engine) -> uqa_storage::diskann_index::format::DiskANNGeneration {
    let table = engine.try_table("diskann_validation").unwrap().unwrap();
    let metadata = table
        .vector_indexes
        .read()
        .get("embedding")
        .unwrap()
        .diskann_query_metadata(&uqa_storage::read_control::StorageReadControl::with_limit(
            1 << 20,
        ))
        .unwrap()
        .unwrap();
    metadata.manifest.input().generation
}

#[test]
fn diskann_existing_index_skip_preserves_physical_state_and_rollback() {
    diskann_existing_name(&Engine::new());
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        diskann_existing_name(&engine);
        assert_eq!(
            definition(&engine, "occupied_diskann"),
            definition(&peer, "occupied_diskann")
        );
        let factory = std::sync::Arc::clone(engine.storage.provider.as_ref().unwrap());
        drop((engine, peer));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(
            sql(&reopened, "SELECT count(*) AS n FROM diskann_validation").rows[0]["n"],
            Value::Int(2)
        );
        let rows = sql(&reopened, "SELECT id,_score FROM diskann_validation WHERE knn_match(embedding,ARRAY[1,0],10) ORDER BY id").rows;
        assert_eq!(rows[0]["_score"], Value::Float(1.0));
        assert_eq!(rows[1]["_score"], Value::Float(0.0));
    }
}
