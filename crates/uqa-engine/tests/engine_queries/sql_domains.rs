//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` evidence for domain identity, coercion, and catalog transactions.

use uqa_core::Value;
use uqa_engine::sql::{format_postgres_text, postgres_result_type};
use uqa_engine::Engine;

fn verify_domains(engine: &Engine) {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/domains_oracle.expected.json"
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
                            Some(
                                format_postgres_text(
                                    value,
                                    result.column_types[index]
                                        .as_ref()
                                        .expect("declared result type"),
                                    Some(engine),
                                )
                                .unwrap(),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let types = result
                .column_types
                .iter()
                .map(|ty| postgres_result_type(ty.as_ref().expect("declared result type")).type_oid)
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
fn domain_semantics_match_postgresql_memory() {
    verify_domains(&Engine::new());
}

#[test]
fn domain_semantics_match_postgresql_sqlite_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("domains.db");
    let engine = Engine::open(&path).unwrap();
    verify_domains(&engine);
    drop(engine);
    let engine = Engine::open(&path).unwrap();
    assert_eq!(
        engine
            .sql("SELECT routine_quoted_local() AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(15)
    );
    assert_eq!(
        engine
            .sql("SELECT routine_domain_array() AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(7)
    );
    assert_eq!(
        engine.sql("SELECT 7::counted AS value", &[]).unwrap().rows[0]["value"],
        Value::Int(7)
    );
    assert_eq!(
        engine
            .sql("SELECT last_value FROM domain_checks", &[])
            .unwrap()
            .rows[0]["last_value"],
        Value::Int(6)
    );
    assert_eq!(
        engine.sql("SELECT 9::positive AS value", &[]).unwrap().rows[0]["value"],
        Value::Int(9)
    );
    assert_eq!(
        engine
            .sql("SELECT 0::positive", &[])
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );
    assert_eq!(
        engine
            .sql("SELECT NULL::positive", &[])
            .unwrap_err()
            .sqlstate(),
        Some("23502")
    );
    assert_eq!(
        engine
            .sql("SELECT 1::rolled_back", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42704")
    );
    assert_eq!(
        engine
            .sql("SELECT routine_domain_local() AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(7)
    );
    assert_eq!(
        engine
            .sql("SELECT routine_domain_identity(12) + 0 AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(12)
    );
    assert_eq!(
        engine
            .sql("SELECT last_value FROM routine_domain_checks", &[])
            .unwrap()
            .rows[0]["last_value"],
        Value::Int(6)
    );
    let session = engine.new_session().unwrap();
    assert_eq!(
        session
            .sql("SELECT 'hello'::domains.\"Mixed.Domain\" AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Str("hell".into())
    );
}
