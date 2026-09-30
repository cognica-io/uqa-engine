//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Replay checked-in `PostgreSQL` 18.4 simple-query oracles: command tags, SQLSTATE and message, DETAIL and HINT when the transcript records them, every result row as `PostgreSQL` text, and the notices and warnings when the transcript records them.

use uqa_core::Value;
use uqa_engine::sql::{format_postgres_text, postgres_result_type};
use uqa_engine::Engine;

/// User-defined type OIDs are database-local, so both transcripts compare them by class.
const FIRST_USER_OID: u32 = 16_384;

fn type_class(oid: u32) -> serde_json::Value {
    if oid >= FIRST_USER_OID {
        serde_json::json!("user-defined")
    } else {
        serde_json::json!(oid)
    }
}

fn normalized_results(results: &serde_json::Value) -> serde_json::Value {
    let mut results = results.clone();
    for result in results.as_array_mut().into_iter().flatten() {
        for oid in result["type_oids"].as_array_mut().into_iter().flatten() {
            *oid = type_class(u32::try_from(oid.as_u64().unwrap()).unwrap());
        }
    }
    results
}

/// Run one simple query and describe it in the oracle's shape.
pub fn run_case(engine: &Engine, sql: &str) -> serde_json::Value {
    engine.take_sql_notices();
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
                            return Ok(None);
                        }
                        format_postgres_text(
                            value,
                            result.column_types[index]
                                .as_ref()
                                .expect("declared result type"),
                            Some(engine),
                        )
                        .map(Some)
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let types = result
            .column_types
            .iter()
            .map(|ty| {
                type_class(
                    postgres_result_type(ty.as_ref().expect("declared result type")).type_oid,
                )
            })
            .collect::<Vec<_>>();
        results
            .push(serde_json::json!({"columns": result.columns, "type_oids": types, "rows": rows}));
        Ok(())
    });
    let error = outcome.err().map(|error| {
        serde_json::json!({
            "sqlstate": error.sqlstate(),
            "message": error.to_string(),
            "detail": error.detail(),
            "hint": error.hint(),
        })
    });
    let notices = engine
        .take_sql_notices()
        .into_iter()
        .map(|notice| {
            serde_json::json!({
                "severity": notice.severity.as_str(),
                "sqlstate": notice.sqlstate,
                "message": notice.message,
                "detail": notice.detail,
                "hint": notice.hint,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({"error": error, "command_tags": tags, "results": results, "notices": notices})
}

/// Compare the diagnostic fields the transcript recorded; transcripts captured without `--details` omit DETAIL and HINT.
fn error_matches(actual: &serde_json::Value, expected: &serde_json::Value) -> bool {
    match (actual.as_object(), expected.as_object()) {
        (Some(actual), Some(expected)) => expected
            .iter()
            .all(|(field, value)| actual.get(field).unwrap_or(&serde_json::Value::Null) == value),
        _ => actual == expected,
    }
}

/// Replay every case of an oracle transcript and panic with each difference.
pub fn verify(engine: &Engine, transcript: &str) {
    let oracle: serde_json::Value = serde_json::from_str(transcript).unwrap();
    assert!(oracle["postgresql_version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18.4"));
    let mut differences = Vec::new();
    for case in oracle["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let actual = run_case(engine, sql);
        let expected_results = normalized_results(&case["results"]);
        // Transcripts captured without `--notices` do not record notices.
        let notices_match = case
            .get("notices")
            .is_none_or(|expected| actual["notices"] == *expected);
        if !error_matches(&actual["error"], &case["error"])
            || actual["command_tags"] != case["command_tags"]
            || (sql != "SELECT version()" && actual["results"] != expected_results)
            || !notices_match
        {
            differences.push(format!(
                "{sql}\nexpected: {}\nactual: {actual}",
                serde_json::json!({"error": case["error"], "command_tags": case["command_tags"], "results": expected_results, "notices": case.get("notices")})
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} of {} cases differ:\n\n{}",
        differences.len(),
        oracle["cases"].as_array().unwrap().len(),
        differences.join("\n\n")
    );
}
