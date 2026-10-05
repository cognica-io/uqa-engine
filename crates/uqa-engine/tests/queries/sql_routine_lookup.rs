//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine lookup for `DROP` and `ALTER` as `LookupFuncWithArgs` performs it: an argument list selects a routine of any kind, which must then be of the command's kind, while a bare name considers only routines of that kind after search-path shadowing. Diagnostics spell the name as written and the argument types as `format_type_be` does.

use uqa_engine::Engine;

fn fails(engine: &Engine, sql: &str, sqlstate: &str, message: &str) -> uqa_sql::SQLError {
    let error = engine.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some(sqlstate), "{sql}: {error}");
    assert_eq!(error.to_string(), message, "{sql}");
    error
}

fn fixture() -> Engine {
    let engine = Engine::new();
    for sql in [
        "CREATE TYPE lookup_mood AS ENUM ('ok')",
        "CREATE FUNCTION lookup_pick(x lookup_mood) RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$",
        "CREATE PROCEDURE lookup_run(x integer) LANGUAGE sql AS $$ SELECT 1 $$",
        "CREATE FUNCTION lookup_twice(x integer) RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$",
        "CREATE FUNCTION lookup_twice(x text) RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$",
        "CREATE SCHEMA lookup_later",
        "CREATE PROCEDURE lookup_later.lookup_pick() LANGUAGE sql AS $$ SELECT 1 $$",
        "SET search_path = public, lookup_later",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    engine
}

#[test]
fn argument_lists_select_routines_of_any_kind_and_spell_types_by_name() {
    let engine = fixture();
    fails(
        &engine,
        "DROP PROCEDURE lookup_pick(lookup_mood)",
        "42809",
        "lookup_pick(lookup_mood) is not a procedure",
    );
    fails(
        &engine,
        "ALTER FUNCTION lookup_run(integer) STABLE",
        "42809",
        "lookup_run(integer) is not a function",
    );
    fails(
        &engine,
        "ALTER FUNCTION lookup_missing(lookup_mood) STABLE",
        "42883",
        "function lookup_missing(lookup_mood) does not exist",
    );
    fails(
        &engine,
        "ALTER ROUTINE lookup_missing(integer) IMMUTABLE",
        "42883",
        "function lookup_missing(integer) does not exist",
    );
    fails(
        &engine,
        "DROP FUNCTION lookup_missing(int4, lookup_mood)",
        "42883",
        "function lookup_missing(integer, lookup_mood) does not exist",
    );
    engine
        .sql("ALTER TYPE lookup_mood RENAME TO lookup_feeling", &[])
        .unwrap();
    fails(
        &engine,
        "DROP PROCEDURE lookup_pick(lookup_feeling)",
        "42809",
        "lookup_pick(lookup_feeling) is not a procedure",
    );
}

#[test]
fn bare_names_consider_only_routines_of_the_command_kind() {
    let engine = fixture();
    fails(
        &engine,
        "DROP FUNCTION lookup_run",
        "42883",
        "could not find a function named \"lookup_run\"",
    );
    let error = fails(
        &engine,
        "DROP FUNCTION lookup_twice",
        "42725",
        "function name \"lookup_twice\" is not unique",
    );
    assert_eq!(
        error.hint(),
        Some("Specify the argument list to select the function unambiguously.")
    );
    // The function in the first schema of the path does not hide the procedure of another signature in a later one.
    engine.sql("DROP PROCEDURE lookup_pick", &[]).unwrap();
    assert!(engine
        .sql(
            "SELECT 1 FROM pg_proc WHERE proname = 'lookup_pick' AND prokind = 'p'",
            &[],
        )
        .unwrap()
        .rows
        .is_empty());
}
