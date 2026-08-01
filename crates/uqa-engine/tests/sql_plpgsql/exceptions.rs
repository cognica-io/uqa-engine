//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn assert_exception_block_rollback(engine: &Engine) {
    exec(engine, "CREATE TABLE exception_log (v INTEGER)");
    exec(
        engine,
        "DO $$
         BEGIN
           INSERT INTO exception_log VALUES (1);
           BEGIN
             INSERT INTO exception_log VALUES (2);
             RAISE EXCEPTION 'discard inner write';
           EXCEPTION
             WHEN OTHERS THEN INSERT INTO exception_log VALUES (3);
           END;
           INSERT INTO exception_log VALUES (4);
         END
         $$",
    );
    let result = exec(engine, "SELECT v FROM exception_log ORDER BY v");
    let values: Vec<_> = result
        .rows
        .iter()
        .map(|row| row.get("v").cloned().unwrap_or(Value::Null))
        .collect();
    assert_eq!(values, vec![Value::Int(1), Value::Int(3), Value::Int(4)]);
}
#[test]
fn raise_notice_formatting_and_sink() {
    let eng = engine();
    // PG17: 'v=% w=%% x=%', 1, 'two' => "v=1 w=% x=two"
    exec(
        &eng,
        "DO $$ BEGIN RAISE NOTICE 'v=% w=%% x=%', 1, 'two'; END $$",
    );
    // PG17: NULL renders as <NULL>.
    exec(
        &eng,
        "DO $$ DECLARE v int; BEGIN RAISE WARNING 'v=%', v; END $$",
    );
    let notices = eng.take_sql_notices();
    assert_eq!(
        notices,
        vec![
            ("NOTICE".to_string(), "v=1 w=% x=two".to_string()),
            ("WARNING".to_string(), "v=<NULL>".to_string()),
        ]
    );
    assert!(eng.take_sql_notices().is_empty());
    // PG17: too few parameters specified for RAISE.
    let err = exec_err(&eng, "DO $$ BEGIN RAISE NOTICE 'v=%'; END $$");
    assert!(
        err.to_string()
            .contains("too few parameters specified for RAISE"),
        "got: {err}"
    );
}

#[test]
fn raise_exception_and_handlers() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION boom(v int) RETURNS int AS $$
         BEGIN
           RAISE EXCEPTION 'bad value %', v;
         END;
         $$ LANGUAGE plpgsql",
    );
    // PG17: message "bad value 42", SQLSTATE P0001.
    let err = exec_err(&eng, "SELECT boom(42) AS v");
    assert_eq!(err.to_string(), "bad value 42");
    assert_eq!(err.sqlstate(), Some("P0001"));
    // EXCEPTION WHEN OTHERS catches it; SQLERRM / SQLSTATE report it.
    exec(
        &eng,
        "CREATE FUNCTION guard(v int) RETURNS text AS $$
         BEGIN
           PERFORM boom(v);
           RETURN 'no error';
         EXCEPTION
           WHEN OTHERS THEN RETURN SQLSTATE || ':' || SQLERRM;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT guard(7) AS v"),
        Value::Str("P0001:bad value 7".into())
    );
    // Named conditions raised explicitly match their handler
    // (PG17: SQLERRM is the condition name, state 22012).
    exec(
        &eng,
        "CREATE FUNCTION div_guard() RETURNS text AS $$
         BEGIN
           RAISE division_by_zero;
         EXCEPTION
           WHEN division_by_zero THEN RETURN SQLSTATE || ':' || SQLERRM;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT div_guard() AS v"),
        Value::Str("22012:division_by_zero".into())
    );
    // Bare RAISE re-throws; outer handler sees the original message.
    exec(
        &eng,
        "CREATE FUNCTION rethrow() RETURNS text AS $$
         BEGIN
           BEGIN
             RAISE EXCEPTION 'inner boom';
           EXCEPTION WHEN OTHERS THEN RAISE;
           END;
           RETURN 'unreachable';
         EXCEPTION
           WHEN OTHERS THEN RETURN 're:' || SQLERRM;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT rethrow() AS v"),
        Value::Str("re:inner boom".into())
    );
    // PG17: bare RAISE outside a handler is an error.
    let err = exec_err(&eng, "DO $$ BEGIN RAISE; END $$");
    assert!(
        err.to_string()
            .contains("RAISE without parameters cannot be used outside an exception handler"),
        "got: {err}"
    );
}

#[test]
fn exception_when_sqlstate_condition() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION state_guard() RETURNS text AS $$
         BEGIN
           RAISE EXCEPTION SQLSTATE '22012';
         EXCEPTION
           WHEN SQLSTATE '22012' THEN RETURN 'caught';
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT state_guard() AS v"),
        Value::Str("caught".into())
    );

    exec(
        &eng,
        "CREATE FUNCTION serialization_guard() RETURNS text AS $$
         BEGIN
           RAISE serialization_failure;
         EXCEPTION
           WHEN serialization_failure THEN RETURN SQLSTATE;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT serialization_guard() AS v"),
        Value::Str("40001".into())
    );
}

#[test]
fn exception_block_rolls_back_database_changes_before_handler() {
    assert_exception_block_rollback(&engine());

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("plpgsql_exception_rollback.db");
    {
        let persistent = Engine::open(&db).unwrap();
        assert_exception_block_rollback(&persistent);
    }
    {
        let reopened = Engine::open(&db).unwrap();
        let result = exec(&reopened, "SELECT v FROM exception_log ORDER BY v");
        let values: Vec<_> = result
            .rows
            .iter()
            .map(|row| row.get("v").cloned().unwrap_or(Value::Null))
            .collect();
        assert_eq!(values, vec![Value::Int(1), Value::Int(3), Value::Int(4)]);
    }
}

#[test]
fn constant_assignment_rejected() {
    let eng = engine();
    // PG17 rejects this at compile time; the engine rejects the
    // assignment when it runs.
    let err = exec_err(
        &eng,
        "DO $$ DECLARE c CONSTANT int := 1; BEGIN c := 2; END $$",
    );
    assert!(
        err.to_string()
            .contains("variable \"c\" is declared CONSTANT"),
        "got: {err}"
    );
}

#[test]
fn control_reached_end_without_return() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION noret() RETURNS int AS $$ BEGIN END; $$ LANGUAGE plpgsql",
    );
    // PG17: control reached end of function without RETURN.
    let err = exec_err(&eng, "SELECT noret() AS v");
    assert!(
        err.to_string()
            .contains("control reached end of function without RETURN"),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------
// Dynamic SQL
// ---------------------------------------------------------------------
