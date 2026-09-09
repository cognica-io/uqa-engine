//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` evidence for command CTE row types, snapshots, and effects.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::ColumnType;

fn verify_command_cte_oracle(engine: &Engine) {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/cte_commands_oracle.expected.json"
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
                    let values = result
                        .positional_rows
                        .as_ref()
                        .and_then(|rows| rows.get(position))
                        .cloned()
                        .unwrap_or_else(|| {
                            result
                                .columns
                                .iter()
                                .map(|column| {
                                    result.rows[position]
                                        .get(column)
                                        .cloned()
                                        .unwrap_or(Value::Null)
                                })
                                .collect()
                        });
                    values
                        .into_iter()
                        .map(|value| match value {
                            Value::Null => None,
                            Value::Str(value) => Some(value),
                            Value::Bool(value) => Some(if value { "t" } else { "f" }.into()),
                            Value::Int(value) => Some(value.to_string()),
                            Value::Float(value) => Some(value.to_string()),
                            value => panic!("unexpected value {value:?} for {sql}"),
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let types = result
                .column_types
                .iter()
                .map(|ty| match ty {
                    Some(ColumnType::Integer) => 23,
                    Some(ColumnType::BigInteger) => 20,
                    Some(ColumnType::Text) => 25,
                    Some(ColumnType::Regtype) => 2206,
                    other => panic!("unexpected result type {other:?} for {sql}"),
                })
                .collect::<Vec<_>>();
            results.push(
                serde_json::json!({ "columns": result.columns, "type_oids": types, "rows": rows }),
            );
            Ok(())
        });
        let error = outcome.err().map(|error| serde_json::json!({ "sqlstate": error.sqlstate(), "message": error.to_string() }));
        let error = serde_json::to_value(error).unwrap();
        let tags = serde_json::to_value(tags).unwrap();
        let results = serde_json::to_value(results).unwrap();
        if error != case["error"]
            || tags != case["command_tags"]
            || (sql != "SELECT version()" && results != case["results"])
        {
            differences.push(format!(
                "{sql}\nexpected: {case}\nactual: error={error}; tags={tags}; results={results}"
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn command_ctes_match_postgresql_memory() {
    verify_command_cte_oracle(&Engine::new());
}

#[test]
fn command_ctes_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("cte_commands.uqa")).unwrap();
    verify_command_cte_oracle(&engine);
}

#[test]
fn command_cte_routine_bindings_survive_reopen_and_rename() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cte_routines.uqa");
    {
        let engine = Engine::open(&path).unwrap();
        engine.sql("CREATE TABLE cte_values (id integer PRIMARY KEY, amount integer);
            INSERT INTO cte_values VALUES (1, 100);
            CREATE FUNCTION cte_bump(value integer) RETURNS integer LANGUAGE SQL IMMUTABLE RETURN value + 1;
            CREATE FUNCTION cte_bump(value text) RETURNS text LANGUAGE SQL IMMUTABLE RETURN value || 'x';
            CREATE FUNCTION cte_saved() RETURNS TABLE(id integer,amount integer) LANGUAGE SQL BEGIN ATOMIC
                WITH changed AS (UPDATE cte_values SET amount=cte_bump(amount) WHERE id=1 RETURNING *)
                SELECT * FROM changed;
            END", &[]).unwrap();
    }
    {
        let engine = Engine::open(&path).unwrap();
        engine
            .sql(
                "ALTER TABLE cte_values RENAME TO cte_renamed; DROP FUNCTION cte_bump(text)",
                &[],
            )
            .unwrap();
        let error = engine
            .sql("DROP FUNCTION cte_bump(integer)", &[])
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("2BP01"));
        let result = engine.sql("SELECT * FROM cte_saved()", &[]).unwrap();
        assert_eq!(
            result.column_types,
            [Some(ColumnType::Integer), Some(ColumnType::Integer)]
        );
        assert_eq!(result.value_at(0, 0), Some(&Value::Int(1)));
        assert_eq!(result.value_at(0, 1), Some(&Value::Int(101)));
    }
    let engine = Engine::open(&path).unwrap();
    let result = engine.sql("SELECT * FROM cte_saved()", &[]).unwrap();
    assert_eq!(result.value_at(0, 1), Some(&Value::Int(102)));
    assert_eq!(
        engine
            .sql("SELECT amount FROM cte_renamed", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(102))
    );
}
