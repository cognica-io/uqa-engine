//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared parameter declarations and execution against a live `PostgreSQL` reference.

use uqa_core::Value;
use uqa_engine::sql::{format_postgres_text, postgres_result_type};
use uqa_engine::Engine;

fn verify_parameters(engine: &Engine) {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/prepared_parameters_oracle.expected.json"
    ))
    .unwrap();
    assert!(oracle["postgresql_version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18.4"));
    let mut differences = Vec::new();
    for case in oracle["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let mut tags = Vec::new();
        let mut results = Vec::new();
        let outcome = engine.sql_simple_query(sql, &[], |result| {
            tags.push(result.command_tag.clone());
            let rows = (0..result.rows.len())
                .map(|position| {
                    result
                        .columns
                        .iter()
                        .enumerate()
                        .map(|(index, column)| {
                            let value = result
                                .positional_rows
                                .as_ref()
                                .and_then(|rows| rows.get(position))
                                .and_then(|row| row.get(index))
                                .or_else(|| result.rows[position].get(column))
                                .unwrap_or(&Value::Null);
                            if matches!(value, Value::Null) {
                                return None;
                            }
                            Some(match result.column_types[index].as_ref() {
                                Some(ty) => format_postgres_text(value, ty, Some(engine)).unwrap(),
                                None => format!("untyped: {value:?}"),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let types = result
                .column_types
                .iter()
                .map(|ty| ty.as_ref().map(|ty| postgres_result_type(ty).type_oid))
                .collect::<Vec<_>>();
            results
                .push(serde_json::json!({"columns":result.columns,"type_oids":types,"rows":rows}));
            Ok(())
        });
        let error = outcome.err().map(
            |error| serde_json::json!({"sqlstate":error.sqlstate(),"message":error.to_string()}),
        );
        let actual = serde_json::json!({"error":error,"command_tags":tags,"results":results});
        if actual["error"] != case["error"]
            || actual["command_tags"] != case["command_tags"]
            || (sql != "SELECT version()" && actual["results"] != case["results"])
        {
            differences.push(format!("{sql}\nexpected: {case}\nactual: {actual}"));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn prepared_parameters_match_postgresql_memory() {
    verify_parameters(&Engine::new());
}

#[test]
fn prepared_parameters_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_parameters(&Engine::open(&directory.path().join("prepared.db")).unwrap());
}
