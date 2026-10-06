//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine body validation under `check_function_bodies`, as `PostgreSQL` 18's `fmgr_sql_validator` and `plpgsql_validator` perform it: with the setting on, `CREATE FUNCTION` analyzes each statement of a SQL body against the catalog and checks the final statement against the declared result; with it off, no body is examined until the routine is called.

#[path = "sql_function_bodies/body_inputs.rs"]
mod body_inputs;

#[path = "sql_function_bodies/body_results.rs"]
mod body_results;

#[path = "sql_function_bodies/series_binding.rs"]
mod series_binding;

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::{SQLError, SQLResult};

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

/// The SQLSTATE, message and detail of an error.
fn reported(error: &SQLError) -> (String, String, Option<String>) {
    let detail = match error {
        SQLError::Diagnostic { detail, .. } => detail.clone(),
        _ => None,
    };
    (
        error.sqlstate().unwrap_or_default().to_string(),
        error.to_string(),
        detail,
    )
}

fn expect(engine: &Engine, statement: &str, sqlstate: &str, message: &str, detail: Option<&str>) {
    assert_eq!(
        reported(&error(engine, statement)),
        (
            sqlstate.to_string(),
            message.to_string(),
            detail.map(str::to_string)
        ),
        "{statement}"
    );
}

fn routine_exists(engine: &Engine, name: &str) -> bool {
    !sql(
        engine,
        &format!("SELECT 1 FROM pg_proc WHERE proname = '{name}'"),
    )
    .rows
    .is_empty()
}

const MISMATCH_INTEGER: &str = "return type mismatch in function declared to return integer";
const MISMATCH_RECORD: &str = "return type mismatch in function declared to return record";
const FINAL_STATEMENT: &str =
    "Function's final statement must be SELECT or INSERT/UPDATE/DELETE/MERGE RETURNING.";

#[test]
fn check_function_bodies_has_its_postgres_setting() {
    let engine = Engine::new();
    let rows = sql(
        &engine,
        "SELECT setting, context, category, short_desc, vartype, boot_val FROM pg_settings WHERE name = 'check_function_bodies'",
    );
    let row = &rows.rows[0];
    assert_eq!(row["setting"], Value::Str("on".into()));
    assert_eq!(row["context"], Value::Str("user".into()));
    assert_eq!(
        row["category"],
        Value::Str("Client Connection Defaults / Statement Behavior".into())
    );
    assert_eq!(
        row["short_desc"],
        Value::Str("Check routine bodies during CREATE FUNCTION and CREATE PROCEDURE.".into())
    );
    assert_eq!(row["vartype"], Value::Str("bool".into()));
    assert_eq!(row["boot_val"], Value::Str("on".into()));
    sql(&engine, "SET check_function_bodies = false");
    assert_eq!(
        sql(&engine, "SHOW check_function_bodies").rows[0]["check_function_bodies"],
        Value::Str("off".into())
    );
}

#[test]
fn create_function_analyzes_each_statement_of_a_sql_body() {
    let engine = Engine::new();
    expect(
        &engine,
        "CREATE FUNCTION later_count() RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM later_table'",
        "42P01",
        "relation \"later_table\" does not exist",
        None,
    );
    // The statements are analyzed, not run, so a table the body creates does not exist for the statements after it.
    expect(
        &engine,
        "CREATE FUNCTION make_and_fill() RETURNS int LANGUAGE sql AS 'CREATE TABLE made_inside (id int); INSERT INTO made_inside VALUES (1); SELECT count(*)::int FROM made_inside'",
        "42P01",
        "relation \"made_inside\" does not exist",
        None,
    );
    expect(
        &engine,
        "CREATE PROCEDURE missing_target() LANGUAGE sql AS 'INSERT INTO nowhere VALUES (1)'",
        "42P01",
        "relation \"nowhere\" does not exist",
        None,
    );
    expect(
        &engine,
        "CREATE FUNCTION calls_later() RETURNS int LANGUAGE sql AS 'SELECT later_fn()'",
        "42883",
        "function later_fn() does not exist",
        None,
    );
    expect(
        &engine,
        "CREATE FUNCTION bad_name(x int) RETURNS int LANGUAGE sql AS 'SELECT y + 1'",
        "42703",
        "column \"y\" does not exist",
        None,
    );
    expect(
        &engine,
        "CREATE FUNCTION bad_position(x int) RETURNS int LANGUAGE sql AS 'SELECT $2'",
        "42P02",
        "there is no parameter $2",
        None,
    );
    expect(
        &engine,
        "CREATE FUNCTION bad_syntax() RETURNS int LANGUAGE sql AS 'SELEC 1'",
        "42601",
        "syntax error at or near \"SELEC\"",
        None,
    );
    for name in [
        "later_count",
        "make_and_fill",
        "missing_target",
        "calls_later",
        "bad_name",
        "bad_position",
        "bad_syntax",
    ] {
        assert!(!routine_exists(&engine, name), "{name}");
    }
    sql(
        &engine,
        "CREATE FUNCTION add_one(x int) RETURNS int LANGUAGE sql AS 'SELECT x + $1'",
    );
    assert_eq!(
        sql(&engine, "SELECT add_one(2) AS v").rows[0]["v"],
        Value::Int(4)
    );
}

#[test]
fn a_polymorphic_sql_body_is_only_parsed() {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE FUNCTION poly_later(anyelement) RETURNS int LANGUAGE sql AS 'SELECT count(*)::int FROM poly_table'",
    );
    expect(
        &engine,
        "CREATE FUNCTION poly_syntax(anyelement) RETURNS int LANGUAGE sql AS 'SELEC 1'",
        "42601",
        "syntax error at or near \"SELEC\"",
        None,
    );
    sql(&engine, "CREATE TABLE poly_table (id int)");
    assert_eq!(
        sql(&engine, "SELECT poly_later(1) AS v").rows[0]["v"],
        Value::Int(0)
    );
}

/// A session with a table to write and a domain to return.
fn result_fixture() -> Engine {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE written (a int)");
    sql(&engine, "CREATE DOMAIN positive AS int CHECK (VALUE > 0)");
    engine
}

#[test]
fn create_function_rejects_a_final_statement_that_does_not_match_the_declared_result() {
    let engine = result_fixture();
    for (statement, message, detail) in [
        (
            "CREATE FUNCTION r_text() RETURNS int LANGUAGE sql AS 'SELECT 1; SELECT ''a'''",
            MISMATCH_INTEGER,
            "Actual return type is text.",
        ),
        (
            "CREATE FUNCTION r_unknown() RETURNS int LANGUAGE sql AS 'SELECT ''1'''",
            MISMATCH_INTEGER,
            "Actual return type is text.",
        ),
        (
            "CREATE FUNCTION r_boolean() RETURNS boolean LANGUAGE sql AS 'SELECT 1'",
            "return type mismatch in function declared to return boolean",
            "Actual return type is integer.",
        ),
        (
            "CREATE FUNCTION r_domain() RETURNS positive LANGUAGE sql AS 'SELECT ''x''::text'",
            "return type mismatch in function declared to return positive",
            "Actual return type is text.",
        ),
        (
            "CREATE FUNCTION r_two() RETURNS int LANGUAGE sql AS 'SELECT 1, 2'",
            MISMATCH_INTEGER,
            "Final statement must return exactly one column.",
        ),
        (
            "CREATE FUNCTION r_many() RETURNS TABLE(a int, b int) LANGUAGE sql AS 'SELECT 1, 2, 3'",
            MISMATCH_RECORD,
            "Final statement returns too many columns.",
        ),
        (
            "CREATE FUNCTION r_few() RETURNS TABLE(a int, b int) LANGUAGE sql AS 'SELECT 1'",
            MISMATCH_RECORD,
            "Final statement returns too few columns.",
        ),
        (
            "CREATE FUNCTION r_out(OUT a int, OUT b text) LANGUAGE sql AS 'SELECT 1'",
            MISMATCH_RECORD,
            "Final statement returns too few columns.",
        ),
        (
            "CREATE FUNCTION r_column() RETURNS TABLE(a int, b int) LANGUAGE sql AS 'SELECT 1, ''x''::text'",
            MISMATCH_RECORD,
            "Final statement returns text instead of integer at column 2.",
        ),
        (
            "CREATE FUNCTION r_empty() RETURNS int LANGUAGE sql AS ''",
            MISMATCH_INTEGER,
            FINAL_STATEMENT,
        ),
        (
            "CREATE FUNCTION r_utility() RETURNS int LANGUAGE sql AS 'CREATE TEMP TABLE scratch (a int)'",
            MISMATCH_INTEGER,
            FINAL_STATEMENT,
        ),
        (
            "CREATE FUNCTION r_insert() RETURNS int LANGUAGE sql AS 'INSERT INTO written VALUES (1)'",
            MISMATCH_INTEGER,
            FINAL_STATEMENT,
        ),
        (
            "CREATE FUNCTION r_before() RETURNS int LANGUAGE sql AS 'SELECT 1; INSERT INTO written VALUES (1)'",
            MISMATCH_INTEGER,
            FINAL_STATEMENT,
        ),
    ] {
        expect(&engine, statement, "42P13", message, Some(detail));
    }
}

#[test]
fn create_function_accepts_results_that_assignment_converts() {
    let engine = result_fixture();
    // Results that assignment coercion converts are accepted, and converted when the function returns.
    for statement in [
        "CREATE FUNCTION a_widened() RETURNS bigint LANGUAGE sql AS 'SELECT 1'",
        "CREATE FUNCTION a_narrowed() RETURNS int LANGUAGE sql AS 'SELECT 1::bigint'",
        "CREATE FUNCTION a_rounded() RETURNS int LANGUAGE sql AS 'SELECT 1.5'",
        "CREATE FUNCTION a_text() RETURNS text LANGUAGE sql AS 'SELECT 1'",
        "CREATE FUNCTION a_domain() RETURNS positive LANGUAGE sql AS 'SELECT 1'",
        "CREATE FUNCTION a_record() RETURNS record LANGUAGE sql AS 'SELECT 1, ''x''::text'",
        "CREATE FUNCTION a_table() RETURNS TABLE(a int, b text) LANGUAGE sql AS 'SELECT 1, 2'",
        "CREATE FUNCTION a_row() RETURNS TABLE(a int, b int) LANGUAGE sql AS 'SELECT ROW(1, 2)'",
        "CREATE FUNCTION a_union() RETURNS SETOF int LANGUAGE sql AS 'SELECT 1 UNION SELECT ''2'''",
        "CREATE FUNCTION a_returning() RETURNS int LANGUAGE sql AS 'INSERT INTO written VALUES (1) RETURNING a'",
        "CREATE FUNCTION a_void() RETURNS void LANGUAGE sql AS ''",
    ] {
        sql(&engine, statement);
    }
    let row = &sql(
        &engine,
        "SELECT a_rounded() AS rounded, a_text() AS text, a_narrowed() AS narrowed",
    )
    .rows[0];
    assert_eq!(row["rounded"], Value::Int(2));
    assert_eq!(row["text"], Value::Str("1".into()));
    assert_eq!(row["narrowed"], Value::Int(1));
    assert_eq!(
        sql(&engine, "SELECT b FROM a_table()").rows[0]["b"],
        Value::Str("2".into())
    );
    // A lone row value is the whole result of a function with several output columns.
    let row = &sql(&engine, "SELECT a, b FROM a_row()").rows[0];
    assert_eq!(
        (row["a"].clone(), row["b"].clone()),
        (Value::Int(1), Value::Int(2))
    );
}

#[test]
fn the_body_is_validated_under_the_functions_own_settings() {
    let engine = Engine::new();
    sql(&engine, "CREATE SCHEMA only_schema");
    sql(&engine, "CREATE TABLE only_schema.only_here (a int)");
    sql(
        &engine,
        "CREATE FUNCTION in_schema() RETURNS bigint LANGUAGE sql SET search_path = only_schema AS 'SELECT count(*) FROM only_here'",
    );
    assert_eq!(
        sql(&engine, "SELECT in_schema() AS v").rows[0]["v"],
        Value::Int(0)
    );
    expect(
        &engine,
        "CREATE FUNCTION outside_schema() RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM only_here'",
        "42P01",
        "relation \"only_here\" does not exist",
        None,
    );
}

#[test]
fn create_function_compiles_a_plpgsql_body() {
    let engine = Engine::new();
    expect(
        &engine,
        "CREATE FUNCTION bad_plpgsql() RETURNS int LANGUAGE plpgsql AS $$ BEGIN RETURN 1 $$",
        "42601",
        "syntax error at end of input",
        None,
    );
    expect(
        &engine,
        "CREATE FUNCTION type_ref() RETURNS int LANGUAGE plpgsql AS $$ DECLARE x later_table.id%TYPE; BEGIN RETURN 1; END $$",
        "42P01",
        "relation \"later_table\" does not exist",
        None,
    );
    // PL/pgSQL compiles the statements it runs only when it first runs them.
    sql(
        &engine,
        "CREATE FUNCTION plpgsql_later() RETURNS bigint LANGUAGE plpgsql AS $$ BEGIN RETURN (SELECT count(*) FROM later_table); END $$",
    );
}

#[test]
fn with_check_function_bodies_off_a_body_is_examined_when_the_routine_is_called() {
    let engine = Engine::new();
    sql(&engine, "SET check_function_bodies = false");
    for statement in [
        "CREATE FUNCTION later_count() RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM later_table'",
        "CREATE FUNCTION bad_sql() RETURNS int LANGUAGE sql AS 'SELEC 1'",
        "CREATE FUNCTION mismatch_later() RETURNS int LANGUAGE sql AS 'SELECT ''x''::text'",
        "CREATE FUNCTION bad_plpgsql() RETURNS int LANGUAGE plpgsql AS $$ BEGIN RETURN 1 $$",
        "CREATE FUNCTION type_ref() RETURNS int LANGUAGE plpgsql AS $$ DECLARE x later_table.id%TYPE; BEGIN RETURN 1; END $$",
        "CREATE PROCEDURE fill_later() LANGUAGE sql AS 'INSERT INTO later_table VALUES (7)'",
    ] {
        sql(&engine, statement);
    }
    // A SQL-standard body belongs to the statement itself, which always analyzes it.
    expect(
        &engine,
        "CREATE FUNCTION standard_later() RETURNS bigint LANGUAGE sql RETURN (SELECT count(*) FROM later_table)",
        "42P01",
        "relation \"later_table\" does not exist",
        None,
    );
    expect(
        &engine,
        "SELECT later_count()",
        "42P01",
        "relation \"later_table\" does not exist",
        None,
    );
    expect(
        &engine,
        "SELECT bad_sql()",
        "42601",
        "syntax error at or near \"SELEC\"",
        None,
    );
    expect(
        &engine,
        "SELECT mismatch_later()",
        "42P13",
        MISMATCH_INTEGER,
        Some("Actual return type is text."),
    );
    expect(
        &engine,
        "SELECT bad_plpgsql()",
        "42601",
        "syntax error at end of input",
        None,
    );
    expect(
        &engine,
        "SELECT type_ref()",
        "42P01",
        "relation \"later_table\" does not exist",
        None,
    );
    expect(
        &engine,
        "CALL fill_later()",
        "42P01",
        "relation \"later_table\" does not exist",
        None,
    );
    sql(&engine, "CREATE TABLE later_table (id int)");
    sql(&engine, "CALL fill_later()");
    let row = &sql(&engine, "SELECT later_count() AS count, type_ref() AS ref").rows[0];
    assert_eq!(row["count"], Value::Int(1));
    assert_eq!(row["ref"], Value::Int(1));
}

#[test]
fn routines_with_unexamined_bodies_survive_a_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bodies.db");
    {
        let engine = Engine::open(&path).unwrap();
        sql(&engine, "SET check_function_bodies = off");
        sql(
            &engine,
            "CREATE FUNCTION later_count() RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM later_table'",
        );
        sql(
            &engine,
            "CREATE FUNCTION bad_sql() RETURNS int LANGUAGE sql AS 'SELEC 1'",
        );
        sql(
            &engine,
            "CREATE FUNCTION bad_plpgsql() RETURNS int LANGUAGE plpgsql AS $$ BEGIN RETURN 1 $$",
        );
        sql(
            &engine,
            "CREATE FUNCTION type_ref() RETURNS int LANGUAGE plpgsql AS $$ DECLARE x later_table.id%TYPE; BEGIN RETURN 1; END $$",
        );
    }
    let engine = Engine::open(&path).unwrap();
    expect(
        &engine,
        "SELECT bad_sql()",
        "42601",
        "syntax error at or near \"SELEC\"",
        None,
    );
    expect(
        &engine,
        "SELECT bad_plpgsql()",
        "42601",
        "syntax error at end of input",
        None,
    );
    sql(&engine, "CREATE TABLE later_table (id int)");
    let row = &sql(&engine, "SELECT later_count() AS count, type_ref() AS ref").rows[0];
    assert_eq!(row["count"], Value::Int(0));
    assert_eq!(row["ref"], Value::Int(1));
    let definitions = sql(
        &engine,
        "SELECT proname::text AS name, prosrc FROM pg_proc WHERE proname IN ('bad_sql', 'later_count') ORDER BY proname",
    );
    assert_eq!(definitions.rows[0]["prosrc"], Value::Str("SELEC 1".into()));
    assert_eq!(
        definitions.rows[1]["prosrc"],
        Value::Str("SELECT count(*) FROM later_table".into())
    );
}

#[test]
fn a_pg_dump_preamble_creates_functions_before_their_tables() {
    let engine = Engine::new();
    let mut results = 0;
    engine
        .sql_simple_query(
            "SET statement_timeout = 0; SET lock_timeout = 0; SET idle_in_transaction_session_timeout = 0; SET transaction_timeout = 0; SET client_encoding = 'UTF8'; SET standard_conforming_strings = on; SELECT pg_catalog.set_config('search_path', '', false); SET check_function_bodies = false; SET xmloption = content; SET client_min_messages = warning; SET row_security = off; \
             CREATE FUNCTION public.item_count() RETURNS bigint LANGUAGE sql AS $$SELECT count(*) FROM public.items$$; \
             SET default_tablespace = ''; SET default_table_access_method = heap; \
             CREATE TABLE public.items (id integer NOT NULL); \
             INSERT INTO public.items VALUES (1), (2);",
            &[],
            |_| {
                results += 1;
                Ok(())
            },
        )
        .unwrap();
    assert!(results > 0);
    sql(&engine, "SET search_path = public");
    assert_eq!(
        sql(&engine, "SELECT item_count() AS v").rows[0]["v"],
        Value::Int(2)
    );
}

#[test]
fn a_body_sees_its_own_routine_and_a_rejected_replacement_keeps_the_old_one() {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE FUNCTION fact(n int) RETURNS int LANGUAGE sql AS 'SELECT CASE WHEN n <= 1 THEN 1 ELSE n * fact(n - 1) END'",
    );
    assert_eq!(
        sql(&engine, "SELECT fact(5) AS v").rows[0]["v"],
        Value::Int(120)
    );
    expect(
        &engine,
        "CREATE OR REPLACE FUNCTION fact(n int) RETURNS int LANGUAGE sql AS 'SELECT ''x''::text'",
        "42P13",
        MISMATCH_INTEGER,
        Some("Actual return type is text."),
    );
    assert_eq!(
        sql(&engine, "SELECT fact(5) AS v").rows[0]["v"],
        Value::Int(120)
    );
}

#[test]
fn a_rejected_body_leaves_no_routine_behind() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rejected.db");
    {
        let engine = Engine::open(&path).unwrap();
        expect(
            &engine,
            "CREATE FUNCTION lost() RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM nowhere'",
            "42P01",
            "relation \"nowhere\" does not exist",
            None,
        );
        sql(&engine, "BEGIN");
        sql(&engine, "SAVEPOINT before_lost");
        expect(
            &engine,
            "CREATE FUNCTION lost() RETURNS int LANGUAGE sql AS 'SELECT ''x''::text'",
            "42P13",
            MISMATCH_INTEGER,
            Some("Actual return type is text."),
        );
        sql(&engine, "ROLLBACK TO before_lost");
        sql(
            &engine,
            "CREATE FUNCTION kept() RETURNS int LANGUAGE sql AS 'SELECT 1'",
        );
        sql(&engine, "COMMIT");
        assert!(!routine_exists(&engine, "lost"));
    }
    let engine = Engine::open(&path).unwrap();
    assert!(!routine_exists(&engine, "lost"));
    assert_eq!(
        sql(&engine, "SELECT kept() AS v").rows[0]["v"],
        Value::Int(1)
    );
}

#[test]
fn a_call_in_a_sql_body_names_a_procedure_without_output_arguments() {
    let engine = Engine::new();
    sql(
        &engine,
        "CREATE PROCEDURE p_out(OUT a int) LANGUAGE sql AS 'SELECT 1'",
    );
    sql(
        &engine,
        "CREATE PROCEDURE p_in(a int) LANGUAGE sql AS 'SELECT 1'",
    );
    let output_arguments =
        "calling procedures with output arguments is not supported in SQL functions";
    expect(
        &engine,
        "CREATE FUNCTION calls_out() RETURNS void LANGUAGE sql AS 'CALL p_out(NULL)'",
        "0A000",
        output_arguments,
        None,
    );
    sql(
        &engine,
        "CREATE FUNCTION calls_in() RETURNS void LANGUAGE sql AS 'CALL p_in(1)'",
    );
    expect(
        &engine,
        "CREATE FUNCTION calls_missing() RETURNS void LANGUAGE sql AS 'CALL nowhere_proc(1)'",
        "42883",
        "procedure nowhere_proc(integer) does not exist",
        None,
    );
    expect(
        &engine,
        "CREATE FUNCTION calls_function() RETURNS void LANGUAGE sql AS 'CALL calls_in()'",
        "42809",
        "calls_in() is not a procedure",
        None,
    );
    sql(&engine, "SET check_function_bodies = off");
    sql(
        &engine,
        "CREATE FUNCTION calls_out() RETURNS void LANGUAGE sql AS 'CALL p_out(NULL)'",
    );
    expect(
        &engine,
        "SELECT calls_out()",
        "0A000",
        output_arguments,
        None,
    );
    sql(&engine, "SELECT calls_in()");
}

#[test]
fn pseudo_types_are_rejected_as_each_language_validator_rejects_them() {
    let engine = Engine::new();
    for (statement, sqlstate, message) in [
        (
            "CREATE FUNCTION f() RETURNS cstring LANGUAGE sql AS 'SELECT 1'",
            "42P13",
            "SQL functions cannot return type cstring",
        ),
        (
            "CREATE FUNCTION f(x internal) RETURNS int LANGUAGE sql AS 'SELECT 1'",
            "42P13",
            "SQL functions cannot have arguments of type internal",
        ),
        (
            "CREATE FUNCTION f(x record) RETURNS int LANGUAGE sql AS 'SELECT 1'",
            "42P13",
            "SQL functions cannot have arguments of type record",
        ),
        (
            "CREATE FUNCTION f() RETURNS cstring LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$",
            "0A000",
            "PL/pgSQL functions cannot return type cstring",
        ),
        (
            "CREATE FUNCTION f(x internal) RETURNS int LANGUAGE plpgsql AS $$ BEGIN RETURN 1; END $$",
            "0A000",
            "PL/pgSQL functions cannot accept type internal",
        ),
        (
            "CREATE FUNCTION f(x int) RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$",
            "42P13",
            "trigger functions cannot have declared arguments",
        ),
    ] {
        expect(&engine, statement, sqlstate, message, None);
    }
    // The declared types are checked whatever check_function_bodies says; the arguments of a trigger function are a body check.
    sql(&engine, "SET check_function_bodies = off");
    expect(
        &engine,
        "CREATE FUNCTION f() RETURNS cstring LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$",
        "0A000",
        "PL/pgSQL functions cannot return type cstring",
        None,
    );
    sql(
        &engine,
        "CREATE FUNCTION tf(x int) RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$",
    );
}

#[test]
fn a_sql_standard_body_is_analyzed_whatever_check_function_bodies_says() {
    let engine = result_fixture();
    for setting in ["on", "off"] {
        sql(&engine, &format!("SET check_function_bodies = {setting}"));
        expect(
            &engine,
            "CREATE FUNCTION std_insert() RETURNS void LANGUAGE sql BEGIN ATOMIC INSERT INTO written (nope) VALUES (1); END",
            "42703",
            "column \"nope\" of relation \"written\" does not exist",
            None,
        );
        expect(
            &engine,
            "CREATE FUNCTION std_missing() RETURNS void LANGUAGE sql BEGIN ATOMIC INSERT INTO nowhere VALUES (1); END",
            "42P01",
            "relation \"nowhere\" does not exist",
            None,
        );
    }
    // Only the final statement's result check follows the setting.
    sql(&engine, "SET check_function_bodies = on");
    expect(
        &engine,
        "CREATE FUNCTION std_mismatch() RETURNS int LANGUAGE sql RETURN 'x'::text",
        "42P13",
        MISMATCH_INTEGER,
        Some("Actual return type is text."),
    );
    sql(&engine, "SET check_function_bodies = off");
    sql(
        &engine,
        "CREATE FUNCTION std_mismatch() RETURNS int LANGUAGE sql RETURN 'x'::text",
    );
    expect(
        &engine,
        "SELECT std_mismatch()",
        "42P13",
        MISMATCH_INTEGER,
        Some("Actual return type is text."),
    );
}
