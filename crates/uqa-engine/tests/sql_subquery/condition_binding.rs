//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::Value;
use uqa_engine::{sql::format_postgres_text, Engine};

#[test]
fn repeated_condition_binding_preserves_postgresql_scopes_types_and_diagnostics() {
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("condition_binding.json")).unwrap();
    assert!(oracle["postgresql_version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18.4"));
    let engine = Engine::new();
    for _ in 0..3 {
        for case in oracle["cases"].as_array().unwrap() {
            let sql = case["sql"].as_str().unwrap();
            let result = engine.sql(sql, &[]);
            if let Some(sqlstate) = case["sqlstate"].as_str() {
                let error = result.unwrap_err();
                assert_eq!(error.sqlstate(), Some(sqlstate), "{sql}");
                assert_eq!(
                    error.to_string(),
                    case["message"].as_str().unwrap(),
                    "{sql}"
                );
                continue;
            }
            let result = result.unwrap_or_else(|error| panic!("{sql}: {error}"));
            assert_eq!(serde_json::json!(result.columns), case["columns"], "{sql}");
            let rows = (0..result.rows.len())
                .map(|row| {
                    (0..result.columns.len())
                        .map(|column| {
                            format_postgres_text(
                                result.value_at(row, column).unwrap(),
                                result.column_types[column].as_ref().unwrap(),
                                Some(&engine),
                            )
                            .unwrap()
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            assert_eq!(serde_json::json!(rows), case["rows"], "{sql}");
        }
    }
    engine
        .sql(
            "PREPARE condition_probe(boolean) AS SELECT 7 AS id WHERE (SELECT $1)",
            &[],
        )
        .unwrap();
    for (parameter, expected) in [
        ("true", Some(7)),
        ("false", None),
        ("NULL", None),
        ("true", Some(7)),
    ] {
        let result = engine
            .sql(&format!("EXECUTE condition_probe({parameter})"), &[])
            .unwrap();
        assert_eq!(result.rows.len(), usize::from(expected.is_some()));
        if let Some(expected) = expected {
            assert_eq!(result.value_at(0, 0), Some(&Value::Int(expected)));
        }
    }
    engine.sql("DEALLOCATE condition_probe", &[]).unwrap();
}
