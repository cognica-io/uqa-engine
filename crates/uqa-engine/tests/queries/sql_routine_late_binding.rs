//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine bodies given as strings resolve names when a session compiles them, as `PostgreSQL`'s backend function cache does: the defining session keeps its PL/pgSQL validation, a SQL-language body compiles when a session first runs it, a session compiling after a rename resolves the names again, and a stored body that no longer compiles cannot keep the catalog from loading.

use uqa_core::Value;
use uqa_engine::Engine;

fn text(engine: &Engine, sql: &str) -> String {
    let result = engine.sql(sql, &[]).unwrap();
    match result.value_at(0, 0) {
        Some(Value::Str(value)) => value.clone(),
        other => panic!("{sql} returned {other:?}"),
    }
}

fn missing_mood(engine: &Engine, sql: &str) {
    let error = engine.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42704"), "{sql}: {error}");
    assert_eq!(error.to_string(), "type \"mood\" does not exist", "{sql}");
}

fn define(engine: &Engine) {
    for sql in [
        "CREATE TYPE mood AS ENUM ('sad', 'ok')",
        "CREATE FUNCTION mood_plpgsql(x mood) RETURNS text LANGUAGE plpgsql AS $$ DECLARE v mood := 'ok'; BEGIN RETURN (x = v)::text; END $$",
        "CREATE FUNCTION mood_sql_early() RETURNS text LANGUAGE sql AS $$ SELECT 'ok'::mood::text $$",
        "CREATE FUNCTION mood_sql_late() RETURNS text LANGUAGE sql AS $$ SELECT 'ok'::mood::text $$",
        "CREATE FUNCTION mood_atomic(x mood) RETURNS boolean LANGUAGE sql RETURN x = 'ok'::mood",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
}

#[test]
fn sessions_keep_the_bodies_they_compiled_across_a_type_rename() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("late.db")).unwrap();
    define(&engine);
    assert_eq!(text(&engine, "SELECT mood_sql_early()"), "ok");
    let sibling = engine.new_session().unwrap();
    engine
        .sql("ALTER TYPE mood RENAME TO feeling", &[])
        .unwrap();
    // The defining session keeps its PL/pgSQL validation and the SQL body it ran.
    assert_eq!(text(&engine, "SELECT mood_plpgsql('ok')"), "true");
    assert_eq!(text(&engine, "SELECT mood_sql_early()"), "ok");
    missing_mood(&engine, "SELECT mood_sql_late()");
    // Another session compiles the bodies after the rename.
    missing_mood(&sibling, "SELECT mood_plpgsql('ok')");
    missing_mood(&sibling, "SELECT mood_sql_early()");
    // A SQL-standard body is bound when defined.
    assert_eq!(
        sibling
            .sql("SELECT mood_atomic('ok') AS ok", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Bool(true))
    );
}

#[test]
fn stored_bodies_compile_when_a_reopened_session_runs_them() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reopen.db");
    {
        let engine = Engine::open(&path).unwrap();
        define(&engine);
        engine
            .sql("ALTER TYPE mood RENAME TO feeling", &[])
            .unwrap();
    }
    let engine = Engine::open(&path).unwrap();
    missing_mood(&engine, "SELECT mood_plpgsql('ok')");
    missing_mood(&engine, "SELECT mood_sql_late()");
    // A compilation that failed is not kept.
    engine
        .sql("ALTER TYPE feeling RENAME TO mood", &[])
        .unwrap();
    assert_eq!(text(&engine, "SELECT mood_plpgsql('ok')"), "true");
    assert_eq!(text(&engine, "SELECT mood_sql_late()"), "ok");
}

#[test]
fn dropping_a_schema_drops_its_enum_types() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("schema.db");
    {
        let engine = Engine::open(&path).unwrap();
        for sql in [
            "CREATE ROLE schema_type_owner",
            "CREATE SCHEMA gone",
            "CREATE TYPE gone.feeling AS ENUM ('ok')",
            "ALTER TYPE gone.feeling OWNER TO schema_type_owner",
        ] {
            engine.sql(sql, &[]).unwrap();
        }
        let error = engine.sql("DROP SCHEMA gone", &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
        assert_eq!(
            error.to_string(),
            "cannot drop schema gone because other objects depend on it"
        );
        engine.sql("DROP SCHEMA gone CASCADE", &[]).unwrap();
        engine.sql("DROP ROLE schema_type_owner", &[]).unwrap();
        assert_eq!(
            engine
                .sql(
                    "SELECT count(*) AS types FROM pg_type WHERE typname IN ('feeling', '_feeling')",
                    &[],
                )
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Int(0))
        );
    }
    let engine = Engine::open(&path).unwrap();
    assert_eq!(
        engine
            .sql(
                "SELECT count(*) AS types FROM pg_type WHERE typname IN ('feeling', '_feeling')",
                &[],
            )
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(0))
    );
}
