//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored routine source aliases after column deletion against a live `PostgreSQL` reference.

use uqa_core::Value;
use uqa_engine::sql::{format_postgres_text, postgres_result_type};
use uqa_engine::Engine;

fn verify_stored_column_alias_drop(engine: &Engine) {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/stored_column_alias_drop_oracle.expected.json"
    ))
    .unwrap();
    assert!(oracle["postgresql_version"]
        .as_str()
        .unwrap()
        .starts_with("PostgreSQL 18.4"));
    let mut differences = Vec::new();
    for (index, case) in oracle["cases"].as_array().unwrap().iter().enumerate() {
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
            differences.push(format!(
                "case {index}: {sql}\nexpected: {case}\nactual: {actual}"
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn stored_column_alias_drop_matches_postgresql_memory() {
    verify_stored_column_alias_drop(&Engine::new());
}

#[test]
fn stored_column_alias_drop_matches_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_stored_column_alias_drop(
        &Engine::open(&directory.path().join("column-drop.db")).unwrap(),
    );
}

#[test]
fn stored_column_alias_drop_matches_postgresql_with_spilled_state() {
    let engine = Engine::new();
    engine.sql("SET work_mem TO '1B'", &[]).unwrap();
    verify_stored_column_alias_drop(&engine);
}

fn alias_value(engine: &Engine, sql: &str) -> Value {
    engine.sql(sql, &[]).unwrap().rows[0]["value"].clone()
}

fn create_alias_lifecycle(engine: &Engine) {
    engine.sql("CREATE TABLE alias_lifecycle(a integer,x integer,b integer,y integer); INSERT INTO alias_lifecycle VALUES(1,42,2,84); CREATE TABLE alias_lifecycle_other(z integer); INSERT INTO alias_lifecycle_other VALUES(7); CREATE FUNCTION alias_lifecycle_reader() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT j.kept+j.other_id+j.k FROM ((alias_lifecycle s CROSS JOIN alias_lifecycle_other o) i(da,kept,db,tail,other_id) CROSS JOIN (VALUES(3)) v(k)) j(da,kept,db,tail,other_id,k); END; CREATE FUNCTION alias_lifecycle_count() RETURNS bigint LANGUAGE SQL BEGIN ATOMIC SELECT count(*) FROM alias_lifecycle; END", &[]).unwrap();
}

#[test]
fn stored_column_alias_drop_retains_inputs_across_public_api_rollback_refresh_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("alias-lifecycle.db");
    let engine = Engine::open(&database).unwrap();
    create_alias_lifecycle(&engine);
    let observer = Engine::open(&database).unwrap();
    let read = "SELECT alias_lifecycle_reader() AS value";
    engine
        .sql(
            "BEGIN; SAVEPOINT original; ALTER TABLE alias_lifecycle DROP COLUMN a, DROP COLUMN b",
            &[],
        )
        .unwrap();
    assert_eq!(alias_value(&engine, read), Value::Int(52));
    engine.sql("ROLLBACK TO original; COMMIT", &[]).unwrap();
    assert_eq!(alias_value(&observer, read), Value::Int(52));
    assert_eq!(
        engine
            .sql(
                "ALTER TABLE alias_lifecycle DROP COLUMN a, DROP COLUMN missing",
                &[]
            )
            .unwrap_err()
            .sqlstate(),
        Some("42703")
    );
    assert_eq!(alias_value(&observer, read), Value::Int(52));
    assert!(engine.drop_column("alias_lifecycle", "a").unwrap());
    assert!(engine.drop_column("alias_lifecycle", "b").unwrap());
    assert_eq!(alias_value(&observer, read), Value::Int(52));
    engine.sql("ALTER TABLE alias_lifecycle ADD COLUMN a integer DEFAULT 1000, ADD COLUMN b integer DEFAULT 2000; ALTER TABLE alias_lifecycle RENAME x TO amount; ALTER TABLE alias_lifecycle RENAME y TO unused_tail", &[]).unwrap();
    assert_eq!(alias_value(&observer, read), Value::Int(52));
    drop(observer);
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    assert_eq!(alias_value(&engine, read), Value::Int(52));
    assert_eq!(
        alias_value(&engine, "SELECT alias_lifecycle_count() AS value"),
        Value::Int(1)
    );
    assert_eq!(
        engine
            .sql(
                "ALTER TABLE alias_lifecycle DROP COLUMN amount RESTRICT",
                &[]
            )
            .unwrap_err()
            .sqlstate(),
        Some("2BP01")
    );
    engine
        .sql(
            "ALTER TABLE alias_lifecycle DROP COLUMN unused_tail RESTRICT",
            &[],
        )
        .unwrap();
    assert_eq!(alias_value(&engine, read), Value::Int(52));
}

#[test]
fn stored_column_alias_drop_cascades_preserve_survivors_after_rollback_and_reopen() {
    for (setup, column, deletion) in [
        (
            "CREATE SEQUENCE alias_owner",
            "regclass GENERATED ALWAYS AS ('alias_owner'::regclass) STORED",
            "DROP SEQUENCE alias_owner CASCADE",
        ),
        (
            "CREATE DOMAIN alias_owner AS integer",
            "alias_owner",
            "DROP DOMAIN alias_owner CASCADE",
        ),
        (
            "CREATE FUNCTION alias_owner() RETURNS integer LANGUAGE SQL IMMUTABLE RETURN 1",
            "integer GENERATED ALWAYS AS (alias_owner()) STORED",
            "DROP FUNCTION alias_owner() CASCADE",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("alias-cascade.db");
        let engine = Engine::open(&database).unwrap();
        engine.sql(&format!("{setup}; CREATE TABLE alias_cascade(a {column},x integer,b {column},y integer); INSERT INTO alias_cascade(x,y) VALUES(42,84); CREATE FUNCTION alias_cascade_reader() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT s.kept+s.tail FROM alias_cascade s(da,kept,db,tail); END; CREATE FUNCTION alias_cascade_doomed() RETURNS boolean LANGUAGE SQL BEGIN ATOMIC SELECT s.da IS NULL FROM alias_cascade s(da,kept,db,tail); END"), &[]).unwrap();
        let observer = Engine::open(&database).unwrap();
        let read = "SELECT alias_cascade_reader() AS value";
        let gone = "SELECT to_regprocedure('alias_cascade_doomed()') IS NULL AS value";
        engine
            .sql(&format!("BEGIN; SAVEPOINT original; {deletion}"), &[])
            .unwrap();
        assert_eq!(alias_value(&engine, read), Value::Int(126));
        assert_eq!(alias_value(&engine, gone), Value::Bool(true));
        engine.sql("ROLLBACK TO original; COMMIT", &[]).unwrap();
        assert_eq!(alias_value(&observer, read), Value::Int(126));
        assert_eq!(alias_value(&observer, gone), Value::Bool(false));
        engine.sql(deletion, &[]).unwrap();
        assert_eq!(alias_value(&observer, read), Value::Int(126));
        assert_eq!(alias_value(&observer, gone), Value::Bool(true));
        drop(observer);
        drop(engine);
        let engine = Engine::open(&database).unwrap();
        assert_eq!(alias_value(&engine, read), Value::Int(126));
        assert_eq!(alias_value(&engine, gone), Value::Bool(true));
        engine.sql("ALTER TABLE alias_cascade ADD COLUMN a integer DEFAULT 1000, ADD COLUMN b integer DEFAULT 2000", &[]).unwrap();
        assert_eq!(alias_value(&engine, read), Value::Int(126));
    }
}

fn remove_source_bindings(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(object) => {
            usize::from(object.remove("bound_columns").is_some())
                + object
                    .values_mut()
                    .map(remove_source_bindings)
                    .sum::<usize>()
        }
        serde_json::Value::Array(items) => items.iter_mut().map(remove_source_bindings).sum(),
        _ => 0,
    }
}

#[test]
fn stored_column_alias_drop_migrates_legacy_sources_without_rebinding_on_reopen() {
    use uqa_storage_sqlite::{Catalog, ManagedConnection};

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("alias-migration.db");
    let engine = Engine::open(&database).unwrap();
    create_alias_lifecycle(&engine);
    drop(engine);
    {
        let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
        let encoded = catalog.get_metadata("sql_functions_json").unwrap().unwrap();
        let mut definitions: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(remove_source_bindings(&mut definitions), 3);
        catalog
            .set_metadata(
                "sql_functions_json",
                &serde_json::to_string(&definitions).unwrap(),
            )
            .unwrap();
    }
    let engine = Engine::open(&database).unwrap();
    let read = "SELECT alias_lifecycle_reader() AS value";
    assert_eq!(alias_value(&engine, read), Value::Int(52));
    engine.sql("ALTER TABLE alias_lifecycle DROP COLUMN a, DROP COLUMN b; ALTER TABLE alias_lifecycle ADD COLUMN a integer DEFAULT 1000, ADD COLUMN b integer DEFAULT 2000", &[]).unwrap();
    assert_eq!(alias_value(&engine, read), Value::Int(52));
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    assert_eq!(alias_value(&engine, read), Value::Int(52));
    drop(engine);
    let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
    let encoded = catalog.get_metadata("sql_functions_json").unwrap().unwrap();
    let mut definitions: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(remove_source_bindings(&mut definitions), 3);
}

#[test]
fn stored_column_alias_drop_migrates_regclass_constants_before_sequence_rename() {
    use uqa_sql::ast::{ColumnDef, Expr};
    use uqa_storage_sqlite::{Catalog, ManagedConnection};

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("alias-regclass-migration.db");
    let engine = Engine::open(&database).unwrap();
    engine.sql("CREATE SEQUENCE alias_migration_sequence; CREATE TABLE alias_migration_source(a regclass GENERATED ALWAYS AS ('alias_migration_sequence'::regclass) STORED,x integer); INSERT INTO alias_migration_source(x) VALUES(42); CREATE FUNCTION alias_migration_reader() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT s.kept FROM alias_migration_source s(da,kept) ORDER BY s.kept LIMIT 1; END", &[]).unwrap();
    drop(engine);
    {
        let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
        let mut tables = catalog.load_tables().unwrap();
        let table = tables
            .iter_mut()
            .find(|table| table.relation.name == "alias_migration_source")
            .unwrap();
        let mut columns: Vec<ColumnDef> = serde_json::from_str(&table.columns_json).unwrap();
        columns[0].generated.as_mut().unwrap().expression = Box::new(Expr::Cast {
            expr: Box::new(Expr::Literal(Value::Str("alias_migration_sequence".into()))),
            ty: "regclass".into(),
        });
        table.columns_json = serde_json::to_string(&columns).unwrap();
        catalog.save_table(table).unwrap();
    }
    let engine = Engine::open(&database).unwrap();
    engine.sql("ALTER SEQUENCE alias_migration_sequence RENAME TO alias_migration_renamed; CREATE SEQUENCE alias_migration_sequence; INSERT INTO alias_migration_source(x) VALUES(84)", &[]).unwrap();
    let rows = engine
        .sql(
            "SELECT a::text AS name FROM alias_migration_source ORDER BY x",
            &[],
        )
        .unwrap();
    assert_eq!(rows.rows.len(), 2);
    for row in &rows.rows {
        assert_eq!(row["name"], Value::Str("alias_migration_renamed".into()));
    }
    engine
        .sql("DROP SEQUENCE alias_migration_sequence RESTRICT", &[])
        .unwrap();
    let observer = Engine::open(&database).unwrap();
    let read = "SELECT alias_migration_reader() AS value";
    engine
        .sql(
            "BEGIN; SAVEPOINT original; DROP SEQUENCE alias_migration_renamed CASCADE",
            &[],
        )
        .unwrap();
    assert_eq!(alias_value(&engine, read), Value::Int(42));
    engine.sql("ROLLBACK TO original; COMMIT", &[]).unwrap();
    assert_eq!(alias_value(&observer, read), Value::Int(42));
    drop(observer);
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    engine
        .sql("DROP SEQUENCE alias_migration_renamed CASCADE", &[])
        .unwrap();
    assert_eq!(alias_value(&engine, read), Value::Int(42));
    drop(engine);
    let engine = Engine::open(&database).unwrap();
    assert_eq!(alias_value(&engine, read), Value::Int(42));
    assert_eq!(
        engine
            .sql("SELECT * FROM alias_migration_source", &[])
            .unwrap()
            .columns,
        vec!["x"]
    );
}
