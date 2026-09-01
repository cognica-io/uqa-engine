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
    // PG18: 'v=% w=%% x=%', 1, 'two' => "v=1 w=% x=two"
    exec(
        &eng,
        "DO $$ BEGIN RAISE NOTICE 'v=% w=%% x=%', 1, 'two'; END $$",
    );
    // PG18: NULL renders as <NULL>.
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
    // PG18: too few parameters specified for RAISE.
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
    // PG18: message "bad value 42", SQLSTATE P0001.
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
    // (PG18: SQLERRM is the condition name, state 22012).
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
    // PG18: bare RAISE outside a handler is an error.
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
fn assert_uses_postgresql_failure_messages_handlers_and_diagnostics() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION assert_true_state() RETURNS text LANGUAGE plpgsql AS $$
         DECLARE rows_before bigint; rows_after bigint;
         BEGIN
           PERFORM 1 WHERE false;
           GET DIAGNOSTICS rows_before = ROW_COUNT;
           ASSERT true, 'unused';
           GET DIAGNOSTICS rows_after = ROW_COUNT;
           RETURN FOUND::text || ':' || rows_before || ':' || rows_after;
         END $$",
    );
    assert_eq!(
        scalar(&eng, "SELECT assert_true_state()"),
        Value::Str("false:0:0".into())
    );

    for (body, expected) in [
        ("ASSERT false", "assertion failed"),
        ("ASSERT NULL::boolean", "assertion failed"),
        ("ASSERT false, 'custom failure'", "custom failure"),
        ("ASSERT false, NULL::text", "assertion failed"),
        ("ASSERT false, 42", "42"),
    ] {
        let error = exec_err(&eng, &format!("DO $$ BEGIN {body}; END $$"));
        assert_eq!(error.sqlstate(), Some("P0004"));
        assert_eq!(error.to_string(), expected);
    }

    exec(
        &eng,
        "CREATE FUNCTION assert_handler() RETURNS text LANGUAGE plpgsql AS $$
         BEGIN
           BEGIN
             ASSERT false;
           EXCEPTION WHEN OTHERS THEN
             RETURN 'caught by others';
           END;
           RETURN 'not caught';
         EXCEPTION WHEN assert_failure THEN
           RETURN SQLSTATE || ':' || SQLERRM;
         END $$",
    );
    assert_eq!(
        scalar(&eng, "SELECT assert_handler()"),
        Value::Str("P0004:assertion failed".into())
    );
}

#[test]
fn plpgsql_boolean_conditions_use_postgresql_assignment_casts() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION boolean_control(value text) RETURNS text LANGUAGE plpgsql AS $$
         BEGIN
           IF value THEN RETURN 'true'; ELSE RETURN 'false'; END IF;
         END $$",
    );
    for (input, expected) in [
        ("true", "true"),
        ("false", "false"),
        ("yes", "true"),
        ("no", "false"),
        ("1", "true"),
        ("0", "false"),
    ] {
        assert_eq!(
            scalar(&eng, &format!("SELECT boolean_control('{input}')")),
            Value::Str(expected.into())
        );
    }
    let error = exec_err(&eng, "SELECT boolean_control('nonsense')");
    assert_eq!(error.sqlstate(), Some("22P02"));
    assert!(error
        .to_string()
        .contains("invalid input syntax for type boolean: \"nonsense\""));
}

#[test]
fn assert_evaluates_messages_lazily_and_honors_check_asserts() {
    let eng = engine();
    exec(&eng, "CREATE SEQUENCE assert_true_condition_sequence");
    exec(&eng, "CREATE SEQUENCE assert_true_message_sequence");
    exec(
        &eng,
        "CREATE FUNCTION assert_true_eval() RETURNS text LANGUAGE plpgsql AS $$
         BEGIN
           ASSERT nextval('assert_true_condition_sequence') > 0,
                  nextval('assert_true_message_sequence')::text;
           RETURN 'ok';
         END $$",
    );
    assert_eq!(
        scalar(&eng, "SELECT assert_true_eval()"),
        Value::Str("ok".into())
    );
    assert_eq!(eng.currval("assert_true_condition_sequence").unwrap(), 1);
    assert!(eng.currval("assert_true_message_sequence").is_err());

    exec(&eng, "CREATE SEQUENCE assert_false_condition_sequence");
    exec(&eng, "CREATE SEQUENCE assert_false_message_sequence");
    exec(
        &eng,
        "CREATE FUNCTION assert_false_eval() RETURNS text LANGUAGE plpgsql AS $$
         BEGIN
           BEGIN
             ASSERT nextval('assert_false_condition_sequence') < 0,
                    nextval('assert_false_message_sequence')::text;
           EXCEPTION WHEN assert_failure THEN RETURN SQLSTATE;
           END;
           RETURN 'not caught';
         END $$",
    );
    assert_eq!(
        scalar(&eng, "SELECT assert_false_eval()"),
        Value::Str("P0004".into())
    );
    assert_eq!(eng.currval("assert_false_condition_sequence").unwrap(), 1);
    assert_eq!(eng.currval("assert_false_message_sequence").unwrap(), 1);

    exec(&eng, "CREATE SEQUENCE assert_off_condition_sequence");
    exec(&eng, "CREATE SEQUENCE assert_off_message_sequence");
    exec(&eng, "SET plpgsql.check_asserts = off");
    exec(
        &eng,
        "DO $$ BEGIN ASSERT nextval('assert_off_condition_sequence') < 0,
                             nextval('assert_off_message_sequence')::text; END $$",
    );
    assert!(eng.currval("assert_off_condition_sequence").is_err());
    assert!(eng.currval("assert_off_message_sequence").is_err());
    exec(&eng, "RESET plpgsql.check_asserts");
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
    // PG18 rejects this at compile time; the engine rejects the
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
    // PG18: control reached end of function without RETURN.
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
