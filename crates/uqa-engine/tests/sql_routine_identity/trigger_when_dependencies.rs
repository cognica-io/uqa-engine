//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn sqlstate(engine: &Engine, sql: &str) -> String {
    engine
        .sql(sql, &[])
        .expect_err("statement must fail")
        .sqlstate()
        .expect("SQLSTATE")
        .to_string()
}

#[test]
fn trigger_when_conditions_keep_exact_routine_dependencies() {
    let engine = Engine::new();
    for ddl in [
        "CREATE TABLE trigger_when_rows(value integer)",
        "CREATE TABLE trigger_when_log(value integer)",
        "CREATE FUNCTION trigger_when_dep(value integer) RETURNS boolean IMMUTABLE RETURN value > 0",
        "CREATE FUNCTION trigger_when_dep(value text) RETURNS boolean IMMUTABLE RETURN false",
        "CREATE FUNCTION trigger_when_fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO trigger_when_log VALUES (NEW.value); RETURN NEW; END $$",
        "CREATE TRIGGER trigger_when_guard BEFORE INSERT ON trigger_when_rows FOR EACH ROW WHEN (trigger_when_dep(NEW.value)) EXECUTE FUNCTION trigger_when_fire()",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    engine
        .sql("DROP FUNCTION trigger_when_dep(text) RESTRICT", &[])
        .unwrap();
    assert_eq!(
        sqlstate(&engine, "DROP FUNCTION trigger_when_dep(integer) RESTRICT"),
        "2BP01"
    );
    engine
        .sql(
            "ALTER FUNCTION trigger_when_dep(integer) RENAME TO trigger_when_dep_renamed",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE FUNCTION trigger_when_dep(value integer) RETURNS boolean IMMUTABLE RETURN false",
            &[],
        )
        .unwrap();
    engine
        .sql("INSERT INTO trigger_when_rows VALUES (1)", &[])
        .unwrap();
    assert_eq!(
        scalar(&engine, "SELECT count(*) AS v FROM trigger_when_log"),
        Value::Int(1)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT position('trigger_when_dep_renamed' in pg_get_triggerdef(oid, true)) > 0 AS v FROM pg_catalog.pg_trigger WHERE tgname = 'trigger_when_guard'"
        ),
        Value::Bool(true)
    );
    engine
        .sql("DROP FUNCTION trigger_when_dep(integer) RESTRICT", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "DROP FUNCTION trigger_when_dep_renamed(integer) RESTRICT"
        ),
        "2BP01"
    );

    engine
        .sql(
            "DROP FUNCTION trigger_when_dep_renamed(integer) CASCADE",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine.take_sql_notices(),
        vec![(
            "NOTICE".into(),
            "drop cascades to trigger trigger_when_guard on table public.trigger_when_rows".into()
        )]
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_trigger WHERE tgname = 'trigger_when_guard'"
        ),
        Value::Int(0)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT to_regprocedure('trigger_when_fire()') IS NOT NULL AS v"
        ),
        Value::Bool(true)
    );
    engine
        .sql("INSERT INTO trigger_when_rows VALUES (2)", &[])
        .unwrap();
    assert_eq!(
        scalar(&engine, "SELECT count(*) AS v FROM trigger_when_rows"),
        Value::Int(2)
    );
}

#[test]
fn replacing_trigger_when_condition_replaces_its_dependency_atomically() {
    let engine = Engine::new();
    for ddl in [
        "CREATE TABLE replace_trigger_when_rows(value integer)",
        "CREATE FUNCTION old_trigger_when_dep(value integer) RETURNS boolean IMMUTABLE RETURN value > 0",
        "CREATE FUNCTION new_trigger_when_dep(value integer) RETURNS boolean IMMUTABLE RETURN value < 100",
        "CREATE FUNCTION replace_trigger_when_fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
        "CREATE TRIGGER replace_trigger_when_guard BEFORE INSERT ON replace_trigger_when_rows FOR EACH ROW WHEN (old_trigger_when_dep(NEW.value)) EXECUTE FUNCTION replace_trigger_when_fire()",
        "CREATE OR REPLACE TRIGGER replace_trigger_when_guard BEFORE INSERT ON replace_trigger_when_rows FOR EACH ROW WHEN (new_trigger_when_dep(NEW.value)) EXECUTE FUNCTION replace_trigger_when_fire()",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    engine
        .sql("DROP FUNCTION old_trigger_when_dep(integer) RESTRICT", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "DROP FUNCTION new_trigger_when_dep(integer) RESTRICT"
        ),
        "2BP01"
    );
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("DROP FUNCTION new_trigger_when_dep(integer) CASCADE", &[])
        .unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_trigger WHERE tgname = 'replace_trigger_when_guard'"
        ),
        Value::Int(0)
    );
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_trigger WHERE tgname = 'replace_trigger_when_guard'"
        ),
        Value::Int(1)
    );
    assert_eq!(
        sqlstate(
            &engine,
            "DROP FUNCTION new_trigger_when_dep(integer) RESTRICT"
        ),
        "2BP01"
    );
}

#[test]
fn trigger_when_routine_binding_survives_reopen_and_old_name_recreation() {
    let directory = TempDir::new().unwrap();
    let database = directory
        .path()
        .join("trigger-when-routine-dependency.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        for ddl in [
            "CREATE TABLE durable_trigger_when_rows(value integer)",
            "CREATE TABLE durable_trigger_when_log(value integer)",
            "CREATE FUNCTION durable_trigger_when_dep(value integer) RETURNS boolean IMMUTABLE RETURN value > 0",
            "CREATE FUNCTION durable_trigger_when_fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO durable_trigger_when_log VALUES (NEW.value); RETURN NEW; END $$",
            "CREATE TRIGGER durable_trigger_when_guard BEFORE INSERT ON durable_trigger_when_rows FOR EACH ROW WHEN (durable_trigger_when_dep(NEW.value)) EXECUTE FUNCTION durable_trigger_when_fire()",
            "ALTER FUNCTION durable_trigger_when_dep(integer) RENAME TO durable_trigger_when_dep_renamed",
            "CREATE FUNCTION durable_trigger_when_dep(value integer) RETURNS boolean IMMUTABLE RETURN false",
        ] {
            engine
                .sql(ddl, &[])
                .unwrap_or_else(|error| panic!("{ddl}: {error}"));
        }
    }

    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql("INSERT INTO durable_trigger_when_rows VALUES (1)", &[])
            .unwrap();
        assert_eq!(
            scalar(
                &engine,
                "SELECT count(*) AS v FROM durable_trigger_when_log"
            ),
            Value::Int(1)
        );
        engine
            .sql(
                "DROP FUNCTION durable_trigger_when_dep(integer) RESTRICT",
                &[],
            )
            .unwrap();
        assert_eq!(
            sqlstate(
                &engine,
                "DROP FUNCTION durable_trigger_when_dep_renamed(integer) RESTRICT"
            ),
            "2BP01"
        );
        engine
            .sql(
                "DROP FUNCTION durable_trigger_when_dep_renamed(integer) CASCADE",
                &[],
            )
            .unwrap();
    }

    let engine = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_catalog.pg_trigger WHERE tgname = 'durable_trigger_when_guard'"
        ),
        Value::Int(0)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT to_regclass('durable_trigger_when_rows') IS NOT NULL AS v"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(
            &engine,
            "SELECT to_regprocedure('durable_trigger_when_fire()') IS NOT NULL AS v"
        ),
        Value::Bool(true)
    );
}

fn remove_expression_function_bindings(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(fields) => {
            let mut removed = fields
                .get_mut("Func")
                .and_then(serde_json::Value::as_object_mut)
                .map_or(0, |function| {
                    usize::from(function.remove("binding").is_some())
                });
            for value in fields.values_mut() {
                removed += remove_expression_function_bindings(value);
            }
            removed
        }
        serde_json::Value::Array(values) => values
            .iter_mut()
            .map(remove_expression_function_bindings)
            .sum(),
        _ => 0,
    }
}

fn remove_trigger_when_binding(catalog_json: &str, trigger_name: &str) -> String {
    let mut catalog: serde_json::Value = serde_json::from_str(catalog_json).unwrap();
    let trigger = catalog["triggers"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|trigger| trigger["definition"]["name"] == trigger_name)
        .unwrap();
    assert_eq!(
        remove_expression_function_bindings(&mut trigger["definition"]["when"]),
        1
    );
    serde_json::to_string(&catalog).unwrap()
}

fn create_trigger_when_migration_fixture(engine: &Engine) {
    for ddl in [
        "CREATE TABLE migration_trigger_when_rows(value integer)",
        "CREATE FUNCTION migration_trigger_when_dep(value integer) RETURNS boolean IMMUTABLE RETURN value > 0",
        "CREATE FUNCTION migration_trigger_when_fire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
        "CREATE TRIGGER migration_trigger_when_guard BEFORE INSERT ON migration_trigger_when_rows FOR EACH ROW WHEN (migration_trigger_when_dep(NEW.value)) EXECUTE FUNCTION migration_trigger_when_fire()",
        "CREATE TABLE migration_trigger_when_rule_source(value integer)",
        "CREATE TABLE migration_trigger_when_rule_log(value integer)",
        "CREATE RULE migration_trigger_when_rule AS ON INSERT TO migration_trigger_when_rule_source DO ALSO INSERT INTO migration_trigger_when_rule_log VALUES (NEW.value)",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
}

#[test]
fn secondary_session_refuses_to_repair_trigger_when_bindings() {
    use uqa_storage::{Catalog, ManagedConnection};

    let directory = TempDir::new().unwrap();
    let database = directory.path().join("trigger-when-load-only.sqlite");
    let engine = Engine::open(&database).unwrap();
    create_trigger_when_migration_fixture(&engine);
    let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
    let encoded = catalog.get_metadata("sql_triggers_json").unwrap().unwrap();
    let legacy = remove_trigger_when_binding(&encoded, "migration_trigger_when_guard");
    catalog.set_metadata("sql_triggers_json", &legacy).unwrap();

    let Err(error) = engine.new_session() else {
        panic!("secondary session must not repair a trigger WHEN binding");
    };
    assert!(error
        .to_string()
        .contains("initial-open routine-identity migration"));
    assert_eq!(
        catalog.get_metadata("sql_triggers_json").unwrap(),
        Some(legacy)
    );
    drop(catalog);
    drop(engine);

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        sqlstate(
            &reopened,
            "DROP FUNCTION migration_trigger_when_dep(integer) RESTRICT"
        ),
        "2BP01"
    );
}

#[test]
fn failed_trigger_when_migration_rolls_back_its_catalog_write() {
    use uqa_storage::{Catalog, ManagedConnection};

    let directory = TempDir::new().unwrap();
    let database = directory
        .path()
        .join("trigger-when-migration-rollback.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        create_trigger_when_migration_fixture(&engine);
    }
    let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
    let encoded = catalog.get_metadata("sql_triggers_json").unwrap().unwrap();
    let legacy = remove_trigger_when_binding(&encoded, "migration_trigger_when_guard");
    catalog.set_metadata("sql_triggers_json", &legacy).unwrap();
    let rules = catalog.get_metadata("sql_rules_json").unwrap().unwrap();
    catalog.set_metadata("sql_rules_json", "{").unwrap();
    drop(catalog);

    assert!(Engine::open(&database).is_err());
    let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
    assert_eq!(
        catalog.get_metadata("sql_triggers_json").unwrap(),
        Some(legacy)
    );
    catalog.set_metadata("sql_rules_json", &rules).unwrap();
    drop(catalog);

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        sqlstate(
            &reopened,
            "DROP FUNCTION migration_trigger_when_dep(integer) RESTRICT"
        ),
        "2BP01"
    );
}
