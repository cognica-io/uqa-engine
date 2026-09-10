//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL value-function types and clocks against a live `PostgreSQL` reference.

use uqa_core::Value;
use uqa_engine::sql::{format_postgres_text, postgres_result_type};
use uqa_engine::Engine;

fn verify_sql_value_clock(engine: &Engine) {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/sql_value_clock_oracle.expected.json"
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
            let fields = result.columns.iter().zip(&result.column_types).map(|(name, ty)| {
                let ty = postgres_result_type(ty.as_ref().expect("declared result type"));
                serde_json::json!({
                    "name":name,"table_oid":0,"column_attribute_number":0,
                    "type_oid":ty.type_oid,"type_size":ty.type_size,
                    "type_modifier":ty.type_modifier,"format":0
                })
            }).collect::<Vec<_>>();
            results.push(serde_json::json!({"columns":result.columns,"type_oids":types,"rows":rows,"fields":fields}));
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
    verify_cursor_message_clocks(engine);
}

fn verify_cursor_message_clocks(engine: &Engine) {
    engine.sql(
        "BEGIN; DECLARE clock_fetch CURSOR FOR SELECT i, statement_timestamp() AS evaluated_at FROM generate_series(1, 3) AS g(i)",
        &[],
    ).unwrap();
    for expected in 1..=3 {
        let mut rows = Vec::new();
        engine
            .sql_simple_query(
                "SELECT statement_timestamp() AS message_time; FETCH NEXT FROM clock_fetch",
                &[],
                |result| {
                    rows.extend(result.rows.iter().cloned());
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["i"], Value::Int(expected));
        assert_eq!(rows[0]["message_time"], rows[1]["evaluated_at"]);
    }
    engine.sql("CLOSE clock_fetch; COMMIT", &[]).unwrap();
}

#[test]
fn sql_value_clock_matches_postgresql_memory() {
    verify_sql_value_clock(&Engine::new());
}

#[test]
fn sql_value_clock_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_sql_value_clock(&Engine::open(&directory.path().join("clocks.db")).unwrap());
}

#[test]
fn sql_value_clock_matches_postgresql_with_spilled_state() {
    let engine = Engine::new();
    engine.sql("SET work_mem TO '1B'", &[]).unwrap();
    verify_sql_value_clock(&engine);
}
