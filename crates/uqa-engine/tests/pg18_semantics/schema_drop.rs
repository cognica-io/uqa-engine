//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema deletion against a live `PostgreSQL` reference.

use uqa_core::Value;
use uqa_engine::sql::{format_postgres_text, postgres_result_type};
use uqa_engine::Engine;

fn verify_schema_drop(engine: &Engine) {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/schema_drop_oracle.expected.json"
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
fn schema_drop_matches_postgresql_memory() {
    verify_schema_drop(&Engine::new());
}

#[test]
fn schema_drop_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_schema_drop(&Engine::open(&directory.path().join("schema-drop.db")).unwrap());
}

#[test]
fn schema_drop_matches_postgresql_with_spilled_state() {
    let engine = Engine::new();
    engine.sql("SET work_mem TO '1B'", &[]).unwrap();
    verify_schema_drop(&engine);
}

#[test]
fn schema_drop_restores_dependencies_after_savepoint_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("schema-rollback.db");
    let engine = Engine::open(&database).unwrap();
    engine.sql("CREATE SCHEMA schema_drop_tx; CREATE DOMAIN schema_drop_tx.d AS integer; CREATE TABLE schema_drop_tx.data (id integer PRIMARY KEY); INSERT INTO schema_drop_tx.data VALUES (7); CREATE SEQUENCE schema_drop_tx.seq START 9; CREATE FUNCTION schema_drop_tx.f() RETURNS integer LANGUAGE SQL RETURN 3; CREATE TABLE schema_drop_retained (keep integer, d schema_drop_tx.d, n integer DEFAULT nextval('schema_drop_tx.seq')); INSERT INTO schema_drop_retained (keep, d) VALUES (1, 2); CREATE VIEW schema_drop_tx_view AS SELECT * FROM schema_drop_tx.data", &[]).unwrap();
    engine
        .sql(
            "BEGIN; SAVEPOINT kept; DROP SCHEMA schema_drop_tx CASCADE; ROLLBACK TO kept; COMMIT",
            &[],
        )
        .unwrap();
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    let row = &engine
        .sql(
            "SELECT id, schema_drop_tx.f() AS f FROM schema_drop_tx_view",
            &[],
        )
        .unwrap()
        .rows[0];
    assert_eq!(row["id"], Value::Int(7));
    assert_eq!(row["f"], Value::Int(3));
    let row = &engine
        .sql(
            "SELECT keep, d::integer AS d, n FROM schema_drop_retained",
            &[],
        )
        .unwrap()
        .rows[0];
    assert_eq!(row["keep"], Value::Int(1));
    assert_eq!(row["d"], Value::Int(2));
    assert_eq!(row["n"], Value::Int(9));
    engine
        .sql("DROP SCHEMA schema_drop_tx CASCADE", &[])
        .unwrap();
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    let result = engine
        .sql("SELECT * FROM schema_drop_retained", &[])
        .unwrap();
    assert_eq!(result.columns, ["keep", "n"]);
    assert_eq!(result.rows[0]["n"], Value::Int(9));
    assert!(engine
        .sql("SELECT * FROM schema_drop_tx_view", &[])
        .is_err());
    let result = engine
        .sql(
            "INSERT INTO schema_drop_retained (keep) VALUES (2) RETURNING n",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0]["n"], Value::Null);
}

#[test]
fn schema_drop_public_remains_absent_after_refresh_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("no-public.db");
    let engine = Engine::open(&database).unwrap();
    let observer = Engine::open(&database).unwrap();
    engine
        .sql(
            "CREATE TABLE public.removed (id integer); DROP SCHEMA public CASCADE",
            &[],
        )
        .unwrap();
    assert!(!observer.has_schema("public").unwrap());
    drop(observer);
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    assert!(!engine.has_schema("public").unwrap());
    engine.sql("CREATE SCHEMA public; CREATE TABLE public.recreated (id integer); INSERT INTO public.recreated VALUES (1)", &[]).unwrap();
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    assert!(engine.has_schema("public").unwrap());
    assert_eq!(
        engine
            .sql("SELECT * FROM public.recreated", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(1)
    );
}
