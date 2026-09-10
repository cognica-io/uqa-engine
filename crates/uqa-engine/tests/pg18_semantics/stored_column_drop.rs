//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored routine column deletion against a live `PostgreSQL` reference.

use uqa_core::Value;
use uqa_engine::sql::{format_postgres_text, postgres_result_type};
use uqa_engine::Engine;

fn verify_stored_column_drop(engine: &Engine) {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/stored_column_drop_oracle.expected.json"
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
fn stored_column_drop_matches_postgresql_memory() {
    verify_stored_column_drop(&Engine::new());
}

#[test]
fn stored_column_drop_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_stored_column_drop(&Engine::open(&directory.path().join("column-drop.db")).unwrap());
}

#[test]
fn stored_column_drop_matches_postgresql_with_spilled_state() {
    let engine = Engine::new();
    engine.sql("SET work_mem TO '1B'", &[]).unwrap();
    verify_stored_column_drop(&engine);
}

fn lifecycle_value(engine: &Engine, sql: &str, column: &str) -> Value {
    engine.sql(sql, &[]).unwrap().rows[0][column].clone()
}

#[test]
fn stored_column_drop_lifecycle_restores_routines_and_views() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("column-drop-lifecycle.db");
    let engine = Engine::open(&database).unwrap();
    engine.sql("CREATE TABLE column_drop_lifecycle(id integer, amount integer, spare integer); INSERT INTO column_drop_lifecycle VALUES(1,42,99); CREATE FUNCTION column_drop_reader() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT amount FROM column_drop_lifecycle; END; CREATE FUNCTION column_drop_keep() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT id FROM column_drop_lifecycle; END; CREATE VIEW column_drop_view AS SELECT column_drop_reader() AS amount", &[]).unwrap();
    let observer = Engine::open(&database).unwrap();
    let read = "SELECT column_drop_reader() AS amount";
    assert_eq!(lifecycle_value(&observer, read, "amount"), Value::Int(42));
    assert!(engine
        .drop_column("column_drop_lifecycle", "amount")
        .is_err());
    assert!(!engine
        .drop_column("column_drop_lifecycle", "missing")
        .unwrap());
    assert_eq!(lifecycle_value(&observer, read, "amount"), Value::Int(42));
    assert!(engine
        .drop_column("column_drop_lifecycle", "spare")
        .unwrap());
    engine.sql("BEGIN; SAVEPOINT keep_reader; ALTER TABLE column_drop_lifecycle DROP COLUMN amount CASCADE", &[]).unwrap();
    let removed = "SELECT to_regprocedure('column_drop_reader()') IS NULL AND to_regclass('column_drop_view') IS NULL AS removed";
    assert_eq!(
        lifecycle_value(&engine, removed, "removed"),
        Value::Bool(true)
    );
    engine.sql("ROLLBACK TO keep_reader; COMMIT", &[]).unwrap();
    assert_eq!(lifecycle_value(&observer, read, "amount"), Value::Int(42));
    assert_eq!(
        lifecycle_value(&observer, "SELECT amount FROM column_drop_view", "amount"),
        Value::Int(42)
    );
    assert_eq!(
        engine
            .sql(
                "ALTER TABLE column_drop_lifecycle DROP COLUMN amount CASCADE, DROP COLUMN missing",
                &[]
            )
            .unwrap_err()
            .sqlstate(),
        Some("42703")
    );
    assert_eq!(lifecycle_value(&observer, read, "amount"), Value::Int(42));
    drop(observer);
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    assert_eq!(lifecycle_value(&engine, read, "amount"), Value::Int(42));
    let observer = Engine::open(&database).unwrap();
    engine
        .sql(
            "ALTER TABLE column_drop_lifecycle DROP COLUMN amount CASCADE",
            &[],
        )
        .unwrap();
    assert_eq!(
        lifecycle_value(&observer, removed, "removed"),
        Value::Bool(true)
    );
    assert_eq!(
        lifecycle_value(&observer, "SELECT column_drop_keep() AS id", "id"),
        Value::Int(1)
    );
    assert_eq!(
        observer
            .sql("SELECT * FROM column_drop_lifecycle", &[])
            .unwrap()
            .columns,
        vec!["id"]
    );
    drop(observer);
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    assert_eq!(
        lifecycle_value(&engine, removed, "removed"),
        Value::Bool(true)
    );
    engine
        .sql(
            "ALTER TABLE column_drop_lifecycle ADD COLUMN amount integer DEFAULT 1000",
            &[],
        )
        .unwrap();
    assert_eq!(
        lifecycle_value(&engine, "SELECT column_drop_keep() AS id", "id"),
        Value::Int(1)
    );
    assert_eq!(
        lifecycle_value(
            &engine,
            "SELECT amount FROM column_drop_lifecycle",
            "amount"
        ),
        Value::Int(1000)
    );
}

#[test]
fn stored_column_drop_lifecycle_public_api_replaces_exact_dependencies() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("column-drop-api.db");
    for engine in [Engine::new(), Engine::open(&database).unwrap()] {
        engine.sql("CREATE TABLE column_drop_api(id integer, amount integer); INSERT INTO column_drop_api VALUES(1,42); CREATE FUNCTION column_drop_api_read() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT amount FROM column_drop_api; END", &[]).unwrap();
        let error = engine.drop_column("column_drop_api", "amount").unwrap_err();
        assert!(error
            .to_string()
            .contains("because other objects depend on it"));
        assert_eq!(
            lifecycle_value(&engine, "SELECT column_drop_api_read() AS value", "value"),
            Value::Int(42)
        );
        engine.sql("CREATE OR REPLACE FUNCTION column_drop_api_read() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT id FROM column_drop_api; END", &[]).unwrap();
        assert!(engine.drop_column("column_drop_api", "amount").unwrap());
        assert_eq!(
            lifecycle_value(&engine, "SELECT column_drop_api_read() AS value", "value"),
            Value::Int(1)
        );
        assert!(!engine.drop_column("column_drop_api", "amount").unwrap());
    }
    let engine = Engine::open(&database).unwrap();
    assert_eq!(
        lifecycle_value(&engine, "SELECT column_drop_api_read() AS value", "value"),
        Value::Int(1)
    );
    assert_eq!(
        engine
            .sql("SELECT * FROM column_drop_api", &[])
            .unwrap()
            .columns,
        vec!["id"]
    );
}

fn create_merge_column_lifecycle(engine: &Engine) {
    engine.sql("CREATE TABLE merge_column_lifecycle(id integer, amount bigint); CREATE SEQUENCE merge_column_counter; INSERT INTO merge_column_lifecycle VALUES(1,42); CREATE FUNCTION merge_column_writer() RETURNS integer LANGUAGE SQL BEGIN ATOMIC MERGE INTO merge_column_lifecycle d USING (VALUES(1),(2)) s(id) ON d.id=s.id WHEN MATCHED THEN UPDATE SET amount=nextval('merge_column_counter') WHEN NOT MATCHED THEN INSERT(id,amount) VALUES(s.id,nextval('merge_column_counter')) RETURNING d.id; END", &[]).unwrap();
}

fn assert_merge_counter(engine: &Engine, called: bool, value: i64) {
    let result = engine
        .sql("SELECT last_value,is_called FROM merge_column_counter", &[])
        .unwrap();
    assert_eq!(result.rows[0]["last_value"], Value::Int(value));
    assert_eq!(result.rows[0]["is_called"], Value::Bool(called));
}

#[test]
fn stored_column_drop_merge_retains_identity_across_rollback_refresh_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("merge-column-lifecycle.db");
    let engine = Engine::open(&database).unwrap();
    create_merge_column_lifecycle(&engine);
    let observer = Engine::open(&database).unwrap();
    let write = "SELECT merge_column_writer() AS id";
    engine
        .sql(
            "BEGIN; SAVEPOINT retain_target; ALTER TABLE merge_column_lifecycle DROP COLUMN amount",
            &[],
        )
        .unwrap();
    assert_eq!(lifecycle_value(&engine, write, "id"), Value::Int(1));
    assert_merge_counter(&engine, false, 1);
    engine
        .sql("ROLLBACK TO retain_target; COMMIT", &[])
        .unwrap();
    assert_eq!(lifecycle_value(&observer, write, "id"), Value::Int(1));
    assert_merge_counter(&observer, true, 2);
    assert!(engine
        .drop_column("merge_column_lifecycle", "amount")
        .unwrap());
    assert_eq!(lifecycle_value(&observer, write, "id"), Value::Int(1));
    assert_merge_counter(&observer, true, 2);
    drop(observer);
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    assert_eq!(lifecycle_value(&engine, write, "id"), Value::Int(1));
    engine.sql("ALTER TABLE merge_column_lifecycle ADD COLUMN amount integer DEFAULT 1000; ALTER TABLE merge_column_lifecycle RENAME COLUMN amount TO replacement; ALTER TABLE merge_column_lifecycle ADD COLUMN amount integer DEFAULT 2000", &[]).unwrap();
    assert_eq!(lifecycle_value(&engine, write, "id"), Value::Int(1));
    assert_merge_counter(&engine, true, 2);
    assert_eq!(
        engine
            .sql("DROP SEQUENCE merge_column_counter RESTRICT", &[])
            .unwrap_err()
            .sqlstate(),
        Some("2BP01")
    );
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    assert_eq!(lifecycle_value(&engine, write, "id"), Value::Int(1));
    let rows = engine
        .sql("SELECT * FROM merge_column_lifecycle ORDER BY id", &[])
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(row["replacement"], Value::Int(1000));
        assert_eq!(row["amount"], Value::Int(2000));
    }
    assert_merge_counter(&engine, true, 2);
}

fn remove_merge_target_bindings(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(object) => {
            usize::from(object.remove("target_column_bindings").is_some())
                + object
                    .values_mut()
                    .map(remove_merge_target_bindings)
                    .sum::<usize>()
        }
        serde_json::Value::Array(array) => array.iter_mut().map(remove_merge_target_bindings).sum(),
        _ => 0,
    }
}

#[test]
fn stored_column_drop_merge_migrates_legacy_target_identities() {
    use uqa_storage_sqlite::{Catalog, ManagedConnection};

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("merge-column-migration.db");
    let engine = Engine::open(&database).unwrap();
    create_merge_column_lifecycle(&engine);
    drop(engine);
    {
        let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
        let encoded = catalog.get_metadata("sql_functions_json").unwrap().unwrap();
        let mut definitions: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(remove_merge_target_bindings(&mut definitions), 1);
        catalog
            .set_metadata(
                "sql_functions_json",
                &serde_json::to_string(&definitions).unwrap(),
            )
            .unwrap();
    }
    let engine = Engine::open(&database).unwrap();
    assert!(engine
        .drop_column("merge_column_lifecycle", "amount")
        .unwrap());
    drop(engine);
    {
        let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
        let encoded = catalog.get_metadata("sql_functions_json").unwrap().unwrap();
        assert!(encoded.contains("target_column_bindings"));
        assert!(encoded.contains("nextval"));
    }
    let engine = Engine::open(&database).unwrap();
    assert_eq!(
        lifecycle_value(&engine, "SELECT merge_column_writer() AS id", "id"),
        Value::Int(1)
    );
    assert_merge_counter(&engine, false, 1);
    assert_eq!(
        engine
            .sql("DROP SEQUENCE merge_column_counter RESTRICT", &[])
            .unwrap_err()
            .sqlstate(),
        Some("2BP01")
    );
}

#[test]
fn stored_column_drop_merge_retains_domain_dependencies_after_target_removal() {
    use uqa_storage_sqlite::{Catalog, ManagedConnection};

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("merge-domain-lifecycle.db");
    let engine = Engine::open(&database).unwrap();
    engine.sql("CREATE DOMAIN merge_original_domain AS integer CHECK(VALUE > 0); CREATE DOMAIN merge_replacement_domain AS integer; CREATE TABLE merge_domain_lifecycle(id integer, amount merge_original_domain DEFAULT 3); INSERT INTO merge_domain_lifecycle VALUES(1,42); CREATE FUNCTION merge_domain_writer() RETURNS integer LANGUAGE SQL BEGIN ATOMIC MERGE INTO merge_domain_lifecycle d USING (VALUES(1)) s(id) ON d.id=s.id WHEN MATCHED THEN UPDATE SET amount=8 RETURNING d.id; END; CREATE FUNCTION merge_domain_default() RETURNS integer LANGUAGE SQL BEGIN ATOMIC MERGE INTO merge_domain_lifecycle d USING (VALUES(1)) s(id) ON d.id=s.id WHEN MATCHED THEN UPDATE SET amount=DEFAULT RETURNING d.id; END", &[]).unwrap();
    drop(engine);
    {
        let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
        let encoded = catalog.get_metadata("sql_functions_json").unwrap().unwrap();
        let mut definitions: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(remove_merge_target_bindings(&mut definitions), 2);
        catalog
            .set_metadata(
                "sql_functions_json",
                &serde_json::to_string(&definitions).unwrap(),
            )
            .unwrap();
    }
    let engine = Engine::open(&database).unwrap();
    let observer = Engine::open(&database).unwrap();
    let write = "SELECT merge_domain_writer() AS id";
    engine.sql("BEGIN; SAVEPOINT retain_domain; ALTER TABLE merge_domain_lifecycle DROP COLUMN amount; ROLLBACK TO retain_domain; COMMIT", &[]).unwrap();
    assert_eq!(lifecycle_value(&observer, write, "id"), Value::Int(1));
    assert!(engine
        .drop_column("merge_domain_lifecycle", "amount")
        .unwrap());
    assert_eq!(lifecycle_value(&observer, write, "id"), Value::Int(1));
    drop(observer);
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    engine.sql("ALTER TABLE merge_domain_lifecycle ADD COLUMN amount merge_replacement_domain DEFAULT 1000", &[]).unwrap();
    assert_eq!(lifecycle_value(&engine, write, "id"), Value::Int(1));
    assert_eq!(
        lifecycle_value(
            &engine,
            "SELECT amount FROM merge_domain_lifecycle",
            "amount"
        ),
        Value::Int(1000)
    );
    engine
        .sql("DROP DOMAIN merge_replacement_domain CASCADE", &[])
        .unwrap();
    assert_eq!(lifecycle_value(&engine, write, "id"), Value::Int(1));
    assert_eq!(
        engine
            .sql("DROP DOMAIN merge_original_domain RESTRICT", &[])
            .unwrap_err()
            .sqlstate(),
        Some("2BP01")
    );
    let observer = Engine::open(&database).unwrap();
    engine.sql("BEGIN; SAVEPOINT keep_writer; DROP DOMAIN merge_original_domain CASCADE; ROLLBACK TO keep_writer; COMMIT", &[]).unwrap();
    assert_eq!(lifecycle_value(&observer, write, "id"), Value::Int(1));
    engine
        .sql("DROP DOMAIN merge_original_domain CASCADE", &[])
        .unwrap();
    let gone = "SELECT to_regprocedure('merge_domain_writer()') IS NULL AS gone";
    assert_eq!(lifecycle_value(&observer, gone, "gone"), Value::Bool(true));
    let default = "SELECT merge_domain_default() AS id";
    assert_eq!(lifecycle_value(&observer, default, "id"), Value::Int(1));
    drop(observer);
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    assert_eq!(lifecycle_value(&engine, gone, "gone"), Value::Bool(true));
    assert_eq!(lifecycle_value(&engine, default, "id"), Value::Int(1));
}
