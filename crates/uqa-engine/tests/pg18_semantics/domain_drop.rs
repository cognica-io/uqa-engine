//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain deletion dependencies against a live `PostgreSQL` reference.

use uqa_core::Value;
use uqa_engine::sql::{format_postgres_text, postgres_result_type};
use uqa_engine::Engine;

fn verify_domain_drop(engine: &Engine) {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/domain_drop_oracle.expected.json"
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
fn domain_drop_matches_postgresql_memory() {
    verify_domain_drop(&Engine::new());
}

#[test]
fn domain_drop_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_domain_drop(&Engine::open(&directory.path().join("domain-drop.db")).unwrap());
}

#[test]
fn domain_drop_matches_postgresql_with_spilled_state() {
    let engine = Engine::new();
    engine.sql("SET work_mem TO '1B'", &[]).unwrap();
    verify_domain_drop(&engine);
}

#[test]
fn domain_drop_restores_dependencies_and_ownership_across_sessions_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("domain-lifecycle.db");
    let engine = Engine::open(&database).unwrap();
    engine.sql("CREATE SCHEMA domain_lifecycle; CREATE ROLE domain_lifecycle_owner; CREATE ROLE domain_lifecycle_next; GRANT USAGE, CREATE ON SCHEMA domain_lifecycle TO domain_lifecycle_owner; SET ROLE domain_lifecycle_owner; CREATE DOMAIN domain_lifecycle.amount AS integer CHECK (VALUE>0); RESET ROLE; CREATE TABLE domain_lifecycle_data (id integer, amount domain_lifecycle.amount); INSERT INTO domain_lifecycle_data VALUES (7, 9); CREATE FUNCTION domain_lifecycle_read() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT amount::integer FROM domain_lifecycle_data; END; CREATE VIEW domain_lifecycle_view AS SELECT domain_lifecycle_read() AS amount", &[]).unwrap();
    let identity = engine
        .sql("SELECT 'domain_lifecycle.amount'::regtype::oid AS oid", &[])
        .unwrap()
        .rows;
    let observer = Engine::open(&database).unwrap();
    let verify_value = "SELECT domain_lifecycle_read() AS amount";
    assert_eq!(
        observer.sql(verify_value, &[]).unwrap().rows[0]["amount"],
        Value::Int(9)
    );
    engine.sql("BEGIN; SAVEPOINT keep_domain; DROP DOMAIN domain_lifecycle.amount CASCADE; ALTER SCHEMA domain_lifecycle OWNER TO domain_lifecycle_next; ROLLBACK TO keep_domain; COMMIT", &[]).unwrap();
    assert_eq!(
        observer.sql(verify_value, &[]).unwrap().rows[0]["amount"],
        Value::Int(9)
    );
    assert_eq!(
        observer
            .sql("SELECT 'domain_lifecycle.amount'::regtype::oid AS oid", &[])
            .unwrap()
            .rows,
        identity
    );
    assert_eq!(
        engine
            .sql("DROP DOMAIN domain_lifecycle.amount", &[])
            .unwrap_err()
            .sqlstate(),
        Some("2BP01")
    );
    drop(observer);
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    assert_eq!(
        engine.sql(verify_value, &[]).unwrap().rows[0]["amount"],
        Value::Int(9)
    );
    let observer = Engine::open(&database).unwrap();
    engine.sql("ALTER SCHEMA domain_lifecycle OWNER TO domain_lifecycle_next; SET ROLE domain_lifecycle_next; DROP DOMAIN domain_lifecycle.amount CASCADE; RESET ROLE", &[]).unwrap();
    let verify_gone = "SELECT to_regtype('domain_lifecycle.amount') IS NULL AS domain_gone, to_regprocedure('domain_lifecycle_read()') IS NULL AS routine_gone, to_regclass('domain_lifecycle_view') IS NULL AS view_gone, (SELECT r.rolname FROM pg_namespace n JOIN pg_roles r ON n.nspowner=r.oid WHERE n.nspname='domain_lifecycle') = 'domain_lifecycle_next' AS owner_changed";
    assert!(observer.sql(verify_gone, &[]).unwrap().rows[0]
        .values()
        .all(|value| value == &Value::Bool(true)));
    let retained = observer
        .sql("SELECT * FROM domain_lifecycle_data", &[])
        .unwrap();
    assert_eq!(retained.columns, ["id"]);
    assert_eq!(retained.rows[0]["id"], Value::Int(7));
    drop(observer);
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    assert!(engine.sql(verify_gone, &[]).unwrap().rows[0]
        .values()
        .all(|value| value == &Value::Bool(true)));
    engine
        .sql(
            "CREATE DOMAIN domain_lifecycle.amount AS integer; DROP DOMAIN domain_lifecycle.amount",
            &[],
        )
        .unwrap();
}
