//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored routine relation dependencies against a live `PostgreSQL` reference.

use uqa_core::Value;
use uqa_engine::sql::{format_postgres_text, postgres_result_type};
use uqa_engine::Engine;

fn verify_stored_relation_drop(engine: &Engine) {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/stored_relation_drop_oracle.expected.json"
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
fn stored_relation_drop_matches_postgresql_memory() {
    verify_stored_relation_drop(&Engine::new());
}

#[test]
fn stored_relation_drop_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_stored_relation_drop(
        &Engine::open(&directory.path().join("stored-relation-drop.db")).unwrap(),
    );
}

#[test]
fn stored_relation_drop_matches_postgresql_with_spilled_state() {
    let engine = Engine::new();
    engine.sql("SET work_mem TO '1B'", &[]).unwrap();
    verify_stored_relation_drop(&engine);
}

#[test]
fn stored_relation_drop_preserves_binding_across_rollback_refresh_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("relation-lifecycle.db");
    let engine = Engine::open(&database).unwrap();
    engine.sql("CREATE TABLE drop_source (id integer); INSERT INTO drop_source VALUES (7); CREATE VIEW drop_bridge AS SELECT id FROM drop_source; CREATE FUNCTION drop_reader() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT id FROM drop_bridge; END; CREATE SEQUENCE drop_sequence START 11; CREATE FUNCTION drop_next() RETURNS bigint LANGUAGE SQL RETURN nextval('drop_sequence'); CREATE FUNCTION drop_default(x regclass DEFAULT 'drop_sequence') RETURNS regclass LANGUAGE SQL AS 'SELECT x'", &[]).unwrap();
    let observer = Engine::open(&database).unwrap();
    assert_eq!(
        observer
            .sql("SELECT drop_reader() AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(7)
    );
    engine.sql("BEGIN; SAVEPOINT keep_dependencies; DROP TABLE drop_source CASCADE; DROP SEQUENCE drop_sequence CASCADE; ROLLBACK TO keep_dependencies; COMMIT", &[]).unwrap();
    assert_eq!(
        observer
            .sql("SELECT drop_reader() AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(7)
    );
    engine.sql("ALTER SEQUENCE drop_sequence RENAME TO drop_moved_sequence; CREATE SEQUENCE drop_sequence START 1000", &[]).unwrap();
    drop(observer);
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    assert_eq!(
        engine.sql("SELECT drop_next() AS value", &[]).unwrap().rows[0]["value"],
        Value::Int(11)
    );
    assert_eq!(
        engine
            .sql("SELECT drop_default()::text AS name", &[])
            .unwrap()
            .rows[0]["name"],
        Value::Str("drop_moved_sequence".into())
    );
    engine.sql("DROP SEQUENCE drop_sequence", &[]).unwrap();
    assert_eq!(
        engine
            .sql("DROP SEQUENCE drop_moved_sequence", &[])
            .unwrap_err()
            .sqlstate(),
        Some("2BP01")
    );
    let observer = Engine::open(&database).unwrap();
    engine
        .sql(
            "DROP TABLE drop_source CASCADE; DROP SEQUENCE drop_moved_sequence CASCADE",
            &[],
        )
        .unwrap();
    let verify = "SELECT to_regclass('drop_bridge') IS NULL AS view_gone, to_regprocedure('drop_reader()') IS NULL AS reader_gone, to_regprocedure('drop_next()') IS NULL AS next_gone, to_regprocedure('drop_default(regclass)') IS NULL AS default_gone";
    assert!(observer.sql(verify, &[]).unwrap().rows[0]
        .values()
        .all(|value| value == &Value::Bool(true)));
    drop(observer);
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    assert!(engine.sql(verify, &[]).unwrap().rows[0]
        .values()
        .all(|value| value == &Value::Bool(true)));
}
