//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Configuration parameters with `PostgreSQL` 18's definitions: values read and shown in their units, the errors of invalid values and contexts, custom placeholders, `set_config`, `SHOW ALL`, `pg_settings`, the search path's text and `$user`, `client_min_messages`, and the parameters `PL/pgSQL` defines when a session loads it. The expected values were captured from `PostgreSQL` 18.4.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::{NoticeLevel, SQLError, SQLResult};

fn sql(engine: &Engine, statement: &str) -> SQLResult {
    engine
        .sql(statement, &[])
        .unwrap_or_else(|error| panic!("{statement}: {error}"))
}

fn error(engine: &Engine, statement: &str) -> SQLError {
    match engine.sql(statement, &[]) {
        Ok(_) => panic!("{statement} succeeded"),
        Err(error) => error,
    }
}

fn text(engine: &Engine, statement: &str) -> Option<String> {
    let result = sql(engine, statement);
    let column = &result.columns[0];
    match &result.rows[0][column] {
        Value::Str(value) => Some(value.clone()),
        Value::Null => None,
        other => panic!("{statement} returned {other:?}"),
    }
}

fn show(engine: &Engine, name: &str) -> (String, String) {
    let result = sql(engine, &format!("SHOW {name}"));
    let column = result.columns[0].clone();
    let Value::Str(value) = &result.rows[0][&column] else {
        panic!("SHOW {name} returned {:?}", result.rows[0]);
    };
    (column, value.clone())
}

fn diagnostic(error: &SQLError) -> (String, Option<String>, Option<String>) {
    match error {
        SQLError::Diagnostic {
            message,
            detail,
            hint,
            ..
        } => (message.clone(), detail.clone(), hint.clone()),
        other => (other.to_string(), None, None),
    }
}

#[test]
fn values_are_read_in_their_units_and_shown_in_the_greatest_unit() {
    let engine = Engine::new();
    assert_eq!(
        show(&engine, "work_mem"),
        ("work_mem".into(), "64MB".into())
    );
    sql(&engine, "SET work_mem = 65536");
    assert_eq!(show(&engine, "work_mem").1, "64MB");
    sql(&engine, "SET work_mem = '1000kB'");
    assert_eq!(show(&engine, "work_mem").1, "1000kB");
    sql(&engine, "SET work_mem = '1.5MB'");
    assert_eq!(show(&engine, "work_mem").1, "1536kB");
    sql(&engine, "SET sort_mem = '2MB'");
    assert_eq!(show(&engine, "sort_mem"), ("work_mem".into(), "2MB".into()));
    assert_eq!(
        text(&engine, "SELECT current_setting('work_mem')").as_deref(),
        Some("2MB")
    );
    assert_eq!(show(&engine, "timezone").0, "TimeZone");
    assert_eq!(show(&engine, "DATESTYLE").0, "DateStyle");
    sql(&engine, "SET enable_indexonlyscan = 'y'");
    assert_eq!(show(&engine, "enable_indexonlyscan").1, "on");
    sql(&engine, "SET client_min_messages = WARNING");
    assert_eq!(show(&engine, "client_min_messages").1, "warning");
    sql(&engine, "SET client_min_messages = debug");
    assert_eq!(show(&engine, "client_min_messages").1, "debug2");
}

#[test]
fn invalid_values_report_postgres_errors() {
    let engine = Engine::new();
    let failure = error(&engine, "SET work_mem = 63");
    assert_eq!(failure.sqlstate(), Some("22023"));
    assert_eq!(
        failure.to_string(),
        "63 kB is outside the valid range for parameter \"work_mem\" (64 kB .. 2147483647 kB)"
    );
    let failure = error(&engine, "SET work_mem = '1 KB'");
    assert_eq!(
        diagnostic(&failure),
        (
            "invalid value for parameter \"work_mem\": \"1 KB\"".into(),
            None,
            Some(
                "Valid units for this parameter are \"B\", \"kB\", \"MB\", \"GB\", and \"TB\"."
                    .into()
            )
        )
    );
    let failure = error(&engine, "SET enable_indexonlyscan = 'maybe'");
    assert_eq!(
        failure.to_string(),
        "parameter \"enable_indexonlyscan\" requires a Boolean value"
    );
    let failure = error(&engine, "SET client_min_messages = 'fatal'");
    assert_eq!(
        diagnostic(&failure).2.as_deref(),
        Some("Available values: debug5, debug4, debug3, debug2, debug1, log, notice, warning, error.")
    );
    let failure = error(&engine, "SET \"SORT_MEM\" = 1");
    assert_eq!(
        failure.to_string(),
        "1 kB is outside the valid range for parameter \"work_mem\" (64 kB .. 2147483647 kB)"
    );
    let failure = error(&engine, "SET work_mem = 1, 2");
    assert_eq!(failure.sqlstate(), Some("22023"));
    assert_eq!(failure.to_string(), "SET work_mem takes only one argument");
    let failure = error(&engine, "SET server_version = '1'");
    assert_eq!(failure.sqlstate(), Some("55P02"));
    assert_eq!(
        failure.to_string(),
        "parameter \"server_version\" cannot be changed"
    );
    assert_eq!(
        error(&engine, "RESET integer_datetimes").to_string(),
        "parameter \"integer_datetimes\" cannot be changed"
    );
    assert_eq!(
        error(&engine, "RESET transaction_isolation").to_string(),
        "parameter \"transaction_isolation\" cannot be reset"
    );
    assert_eq!(
        error(&engine, "SET default_with_oids = true").to_string(),
        "tables declared WITH OIDS are not supported"
    );
    sql(&engine, "SET default_with_oids = false");
}

#[test]
fn custom_parameters_become_placeholders_that_outlive_their_transaction() {
    let engine = Engine::new();
    let failure = error(&engine, "SHOW my.var");
    assert_eq!(failure.sqlstate(), Some("42704"));
    assert_eq!(
        text(&engine, "SELECT current_setting('my.var', true)"),
        None
    );
    sql(&engine, "SET my.var = 1");
    assert_eq!(show(&engine, "my.var"), ("my.var".into(), "1".into()));
    sql(&engine, "SET \"my.Var5\" = 'x'");
    assert_eq!(show(&engine, "my.var5"), ("my.Var5".into(), "x".into()));
    sql(&engine, "BEGIN");
    sql(&engine, "SET my.var3 = 'q'");
    sql(&engine, "ROLLBACK");
    assert_eq!(
        text(&engine, "SELECT current_setting('my.var3')").as_deref(),
        Some("")
    );
    sql(&engine, "RESET my.never");
    assert_eq!(show(&engine, "my.never").1, "");
    sql(&engine, "SET a.b.c = 2");
    assert_eq!(show(&engine, "a.b.c").1, "2");
    let failure = error(&engine, "SET my = 1");
    assert_eq!(failure.sqlstate(), Some("42704"));
    assert_eq!(
        failure.to_string(),
        "unrecognized configuration parameter \"my\""
    );
    let failure = error(&engine, "SET \"1a.b\" = 1");
    assert_eq!(failure.sqlstate(), Some("42602"));
    assert_eq!(
        diagnostic(&failure),
        (
            "invalid configuration parameter name \"1a.b\"".into(),
            Some(
                "Custom parameter names must be two or more simple identifiers separated by dots."
                    .into()
            ),
            None
        )
    );
    sql(&engine, "SET my.z = 'kept'");
    sql(&engine, "RESET ALL");
    assert_eq!(show(&engine, "my.z").1, "");
}

#[test]
fn set_config_assigns_for_the_session_or_the_transaction() {
    let engine = Engine::new();
    assert_eq!(
        text(&engine, "SELECT set_config('work_mem', '65536', false)").as_deref(),
        Some("64MB")
    );
    assert_eq!(
        text(&engine, "SELECT set_config('my.var', 'x', false)").as_deref(),
        Some("x")
    );
    assert_eq!(
        text(&engine, "SELECT set_config('my.var', 'y', NULL)").as_deref(),
        Some("y")
    );
    assert_eq!(show(&engine, "my.var").1, "y");
    let result = sql(
        &engine,
        "SELECT set_config('my.var', 'z', true) AS assigned, current_setting('my.var') AS seen",
    );
    assert_eq!(result.rows[0]["assigned"], Value::Str("z".into()));
    assert_eq!(result.rows[0]["seen"], Value::Str("z".into()));
    assert_eq!(show(&engine, "my.var").1, "y");
    assert_eq!(engine.take_sql_notices().len(), 0);
    sql(&engine, "BEGIN");
    sql(&engine, "SELECT set_config('my.var2', 'local', true)");
    assert_eq!(show(&engine, "my.var2").1, "local");
    sql(&engine, "COMMIT");
    assert_eq!(show(&engine, "my.var2").1, "");
    assert_eq!(
        text(&engine, "SELECT set_config('my.var', NULL, false)").as_deref(),
        Some("")
    );
    let failure = error(&engine, "SELECT set_config(NULL, 'x', false)");
    assert_eq!(failure.sqlstate(), Some("22004"));
    assert_eq!(failure.to_string(), "SET requires parameter name");
    let failure = error(&engine, "SELECT set_config('nosuch', 'x', false)");
    assert_eq!(failure.sqlstate(), Some("42704"));
    let failure = error(&engine, "SELECT set_config('work_mem', 'abc', false)");
    assert_eq!(
        failure.to_string(),
        "invalid value for parameter \"work_mem\": \"abc\""
    );
}

#[test]
fn search_path_keeps_its_text_and_resolves_user_to_the_current_role() {
    let engine = Engine::new();
    assert_eq!(show(&engine, "search_path").1, "\"$user\", public");
    assert_eq!(engine.search_path(), vec!["$user", "public"]);
    sql(
        &engine,
        "SET search_path = 'a$b', public, 'My S', \"$user\"",
    );
    assert_eq!(
        show(&engine, "search_path").1,
        "\"a$b\", public, \"My S\", \"$user\""
    );
    sql(&engine, "SET search_path = 'a,b'");
    assert_eq!(show(&engine, "search_path").1, "\"a,b\"");
    sql(&engine, "SET search_path = ''");
    assert_eq!(show(&engine, "search_path").1, "\"\"");
    sql(
        &engine,
        "SELECT set_config('search_path', 'A, \"B\" ,c', false)",
    );
    assert_eq!(show(&engine, "search_path").1, "A, \"B\" ,c");
    assert_eq!(engine.search_path(), vec!["a", "B", "c"]);
    let failure = error(&engine, "SELECT set_config('search_path', 'a,,b', false)");
    assert_eq!(
        diagnostic(&failure),
        (
            "invalid value for parameter \"search_path\": \"a,,b\"".into(),
            Some("List syntax is invalid.".into()),
            None
        )
    );
    sql(&engine, "SET search_path TO DEFAULT");
    assert_eq!(show(&engine, "search_path").1, "\"$user\", public");
    sql(&engine, "CREATE SCHEMA uqa");
    sql(&engine, "CREATE TABLE in_user_schema (a int)");
    assert_eq!(
        text(
            &engine,
            "SELECT relnamespace::regnamespace::text FROM pg_class WHERE relname = 'in_user_schema'"
        )
        .as_deref(),
        Some("uqa")
    );
    let schemas = sql(&engine, "SELECT current_schemas(false) AS schemas");
    assert_eq!(
        schemas.rows[0]["schemas"],
        Value::Array(
            uqa_core::ArrayValue::try_new(vec![
                Value::Str("uqa".into()),
                Value::Str("public".into())
            ])
            .unwrap()
        )
    );
}

#[test]
fn an_empty_search_path_finds_no_schema_to_create_in() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE visible (a int)");
    sql(
        &engine,
        "SELECT pg_catalog.set_config('search_path', '', false)",
    );
    assert_eq!(show(&engine, "search_path").1, "");
    let failure = error(&engine, "CREATE TABLE elsewhere (a int)");
    assert_eq!(failure.sqlstate(), Some("3F000"));
    assert_eq!(
        failure.to_string(),
        "no schema has been selected to create in"
    );
    assert_eq!(
        error(&engine, "SELECT * FROM visible").sqlstate(),
        Some("42P01")
    );
    assert_eq!(text(&engine, "SELECT current_schema()"), None);
    sql(&engine, "SELECT count(*) FROM public.visible");
}

#[test]
fn show_all_and_pg_settings_report_postgres_rows() {
    let engine = Engine::new();
    let all = sql(&engine, "SHOW ALL");
    assert_eq!(all.columns, ["name", "setting", "description"]);
    let names = all
        .rows
        .iter()
        .map(|row| match &row["name"] {
            Value::Str(name) => name.clone(),
            other => panic!("{other:?}"),
        })
        .collect::<Vec<_>>();
    let mut sorted = names.clone();
    sorted.sort_by_key(|name| name.to_ascii_lowercase());
    assert_eq!(names, sorted);
    for hidden in [
        "role",
        "session_authorization",
        "is_superuser",
        "default_with_oids",
        "plpgsql.check_asserts",
    ] {
        assert!(!names.iter().any(|name| name == hidden), "{hidden}");
    }
    let work_mem = all
        .rows
        .iter()
        .find(|row| row["name"] == Value::Str("work_mem".into()))
        .unwrap();
    assert_eq!(work_mem["setting"], Value::Str("64MB".into()));
    assert_eq!(
        work_mem["description"],
        Value::Str("Sets the maximum memory to be used for query workspaces.".into())
    );
    let row = sql(
        &engine,
        "SELECT setting, unit, vartype, context, min_val, max_val, boot_val, reset_val, source FROM pg_settings WHERE name = 'work_mem'",
    );
    let expected = [
        ("setting", "65536"),
        ("unit", "kB"),
        ("vartype", "integer"),
        ("context", "user"),
        ("min_val", "64"),
        ("max_val", "2147483647"),
        ("boot_val", "4096"),
        ("reset_val", "65536"),
        ("source", "default"),
    ];
    for (column, value) in expected {
        assert_eq!(row.rows[0][column], Value::Str(value.into()), "{column}");
    }
    sql(&engine, "SET work_mem = '2MB'");
    assert_eq!(
        text(
            &engine,
            "SELECT source FROM pg_settings WHERE name = 'work_mem'"
        )
        .as_deref(),
        Some("session")
    );
    assert_eq!(
        text(&engine, "SELECT array_to_string(enumvals, ',') FROM pg_settings WHERE name = 'client_min_messages'").as_deref(),
        Some("debug5,debug4,debug3,debug2,debug1,log,notice,warning,error")
    );
    assert_eq!(
        text(
            &engine,
            "SELECT source FROM pg_settings WHERE name = 'transaction_isolation'"
        )
        .as_deref(),
        Some("override")
    );
    sql(&engine, "BEGIN ISOLATION LEVEL SERIALIZABLE");
    let row = sql(
        &engine,
        "SELECT setting, reset_val, source FROM pg_settings WHERE name = 'transaction_isolation'",
    );
    assert_eq!(row.rows[0]["setting"], Value::Str("serializable".into()));
    assert_eq!(
        row.rows[0]["reset_val"],
        Value::Str("read committed".into())
    );
    assert_eq!(row.rows[0]["source"], Value::Str("session".into()));
    sql(&engine, "COMMIT");
}

#[test]
fn client_settings_are_what_reset_restores() {
    let engine = Engine::new();
    engine
        .set_client_parameter("application_name", "psql")
        .unwrap();
    assert_eq!(show(&engine, "application_name").1, "psql");
    sql(&engine, "SET application_name = 'x'");
    let row = sql(
        &engine,
        "SELECT setting, reset_val, source FROM pg_settings WHERE name = 'application_name'",
    );
    assert_eq!(row.rows[0]["setting"], Value::Str("x".into()));
    assert_eq!(row.rows[0]["reset_val"], Value::Str("psql".into()));
    assert_eq!(row.rows[0]["source"], Value::Str("session".into()));
    sql(&engine, "RESET application_name");
    assert_eq!(show(&engine, "application_name").1, "psql");
    assert_eq!(
        text(
            &engine,
            "SELECT source FROM pg_settings WHERE name = 'application_name'"
        )
        .as_deref(),
        Some("client")
    );
    sql(&engine, "SET application_name = 'caf\u{e9}'");
    assert_eq!(show(&engine, "application_name").1, "caf\\xc3\\xa9");
    sql(
        &engine,
        &format!("SET application_name = '{}'", "x".repeat(70)),
    );
    assert_eq!(show(&engine, "application_name").1, "x".repeat(63));
    let notices = engine.take_sql_notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].sqlstate, "42622");
    assert_eq!(
        notices[0].message,
        format!(
            "identifier \"{}\" will be truncated to \"{}\"",
            "x".repeat(70),
            "x".repeat(63)
        )
    );
}

#[test]
fn client_min_messages_withholds_notices_as_they_are_raised() {
    let engine = Engine::new();
    let levels = |engine: &Engine| {
        engine
            .take_sql_notices()
            .into_iter()
            .map(|notice| (notice.level, notice.message))
            .collect::<Vec<_>>()
    };
    let raise_all = "DO $$ BEGIN RAISE DEBUG 'dbg'; RAISE LOG 'log'; RAISE INFO 'info'; RAISE NOTICE 'notice'; RAISE WARNING 'warning'; END $$";
    sql(&engine, raise_all);
    assert_eq!(
        levels(&engine),
        [
            (NoticeLevel::Info, "info".to_string()),
            (NoticeLevel::Notice, "notice".to_string()),
            (NoticeLevel::Warning, "warning".to_string()),
        ]
    );
    sql(&engine, "SET client_min_messages = error");
    sql(&engine, raise_all);
    assert_eq!(levels(&engine), [(NoticeLevel::Info, "info".to_string())]);
    sql(&engine, "SET client_min_messages = debug1");
    sql(&engine, raise_all);
    assert_eq!(levels(&engine).len(), 5);
    sql(&engine, "RESET client_min_messages");
    sql(
        &engine,
        "CREATE FUNCTION quiet() RETURNS void LANGUAGE plpgsql SET client_min_messages = warning AS $$ BEGIN RAISE NOTICE 'inside'; END $$",
    );
    sql(&engine, "SELECT quiet()");
    sql(&engine, "DO $$ BEGIN RAISE NOTICE 'after'; END $$");
    assert_eq!(
        levels(&engine),
        [(NoticeLevel::Notice, "after".to_string())]
    );
    sql(&engine, "BEGIN");
    sql(&engine, "SET client_min_messages = warning");
    sql(&engine, "ROLLBACK");
    sql(&engine, "DROP TABLE IF EXISTS no_such_table");
    assert_eq!(
        levels(&engine),
        [(
            NoticeLevel::Notice,
            "table \"no_such_table\" does not exist, skipping".to_string()
        )]
    );
    sql(&engine, "SET client_min_messages = warning");
    sql(&engine, "DROP TABLE IF EXISTS no_such_table");
    assert_eq!(levels(&engine).len(), 0);
}

#[test]
fn plpgsql_defines_its_parameters_when_the_session_loads_it() {
    let engine = Engine::new();
    assert_eq!(
        error(&engine, "SHOW plpgsql.check_asserts").to_string(),
        "unrecognized configuration parameter \"plpgsql.check_asserts\""
    );
    sql(&engine, "SET plpgsql.nosuch = 1");
    sql(&engine, "SET plpgsql.check_asserts = off");
    sql(&engine, "DO $$ BEGIN END $$");
    let notices = engine.take_sql_notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].level, NoticeLevel::Warning);
    assert_eq!(notices[0].sqlstate, "42602");
    assert_eq!(
        notices[0].message,
        "invalid configuration parameter name \"plpgsql.nosuch\", removing it"
    );
    assert_eq!(
        notices[0].detail.as_deref(),
        Some("\"plpgsql\" is now a reserved prefix.")
    );
    assert_eq!(show(&engine, "plpgsql.check_asserts").1, "off");
    assert_eq!(
        error(&engine, "SHOW plpgsql.nosuch").sqlstate(),
        Some("42704")
    );
    let failure = error(&engine, "SET plpgsql.nosuch2 = 1");
    assert_eq!(
        diagnostic(&failure),
        (
            "invalid configuration parameter name \"plpgsql.nosuch2\"".into(),
            Some("\"plpgsql\" is a reserved prefix.".into()),
            None
        )
    );
    assert_eq!(
        text(
            &engine,
            "SELECT source FROM pg_settings WHERE name = 'plpgsql.check_asserts'"
        )
        .as_deref(),
        Some("session")
    );
}

#[test]
fn routine_configuration_keeps_canonical_names_and_values_as_written() {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE FUNCTION configured() RETURNS int LANGUAGE sql SET work_mem = '1024kB' SET datestyle = 'iso, mdy' SET search_path = a, \"B\" SET \"SORT_MEM\" = 2048 SET my.x = 'v' AS 'SELECT 1'",
    );
    assert_eq!(
        text(
            &engine,
            "SELECT array_to_string(proconfig, '|') FROM pg_proc WHERE proname = 'configured'"
        )
        .as_deref(),
        Some("work_mem=2048|DateStyle=iso, mdy|search_path=a, \"B\"|my.x=v")
    );
    sql(&engine, "SET work_mem = '3MB'");
    sql(
        &engine,
        "CREATE OR REPLACE FUNCTION configured() RETURNS int LANGUAGE sql SET work_mem FROM CURRENT AS 'SELECT 1'",
    );
    assert_eq!(
        text(
            &engine,
            "SELECT array_to_string(proconfig, '|') FROM pg_proc WHERE proname = 'configured'"
        )
        .as_deref(),
        Some("work_mem=3MB")
    );
}

#[test]
fn defaults_for_new_relations_name_existing_tablespaces_and_table_methods() {
    let engine = Engine::new();
    sql(&engine, "SET default_tablespace = ''");
    sql(&engine, "SET default_tablespace = 'pg_default'");
    assert_eq!(show(&engine, "default_tablespace").1, "pg_default");
    assert_eq!(
        diagnostic(&error(&engine, "SET default_tablespace = 'nosuch'")),
        (
            "invalid value for parameter \"default_tablespace\": \"nosuch\"".into(),
            Some("Tablespace \"nosuch\" does not exist.".into()),
            None
        )
    );
    sql(&engine, "SET default_table_access_method = heap");
    assert_eq!(
        diagnostic(&error(
            &engine,
            "SET default_table_access_method = 'nosuch'"
        )),
        (
            "invalid value for parameter \"default_table_access_method\": \"nosuch\"".into(),
            Some("Table access method \"nosuch\" does not exist.".into()),
            None
        )
    );
    let failure = error(&engine, "SET default_table_access_method = 'btree'");
    assert_eq!(failure.sqlstate(), Some("55000"));
    assert_eq!(
        failure.to_string(),
        "access method \"btree\" is not of type TABLE"
    );
    assert_eq!(
        diagnostic(&error(&engine, "SET default_table_access_method = ''"))
            .1
            .as_deref(),
        Some("\"default_table_access_method\" cannot be empty.")
    );
    sql(&engine, "SET xmloption = document");
    assert_eq!(show(&engine, "xmloption").1, "document");
    sql(&engine, "SET row_security = off");
    assert_eq!(show(&engine, "row_security").1, "off");
    sql(&engine, "SET standard_conforming_strings = on");
    sql(&engine, "SET escape_string_warning = off");
}

#[test]
fn is_superuser_follows_the_role_the_session_sets() {
    let engine = Engine::new();
    assert_eq!(show(&engine, "is_superuser").1, "on");
    sql(&engine, "CREATE ROLE plain_role");
    sql(&engine, "SET ROLE plain_role");
    assert_eq!(show(&engine, "is_superuser").1, "off");
    assert_eq!(show(&engine, "role").1, "plain_role");
    let failure = error(&engine, "SET session_replication_role = replica");
    assert_eq!(failure.sqlstate(), Some("42501"));
    assert_eq!(
        failure.to_string(),
        "permission denied to set parameter \"session_replication_role\""
    );
    sql(&engine, "RESET ROLE");
    assert_eq!(show(&engine, "is_superuser").1, "on");
}

#[test]
fn quote_ident_quotes_dollar_signs() {
    let engine = Engine::new();
    assert_eq!(
        text(&engine, "SELECT quote_ident('a$b')").as_deref(),
        Some("\"a$b\"")
    );
    assert_eq!(
        text(&engine, "SELECT quote_ident('_x1')").as_deref(),
        Some("_x1")
    );
}
