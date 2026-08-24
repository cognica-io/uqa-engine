//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

// ---------------------------------------------------------------------
// Scalar functions
// ---------------------------------------------------------------------

#[test]
fn plpgsql_scalar_arithmetic() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION add_em(a integer, b integer) RETURNS integer AS $$
         BEGIN
           RETURN a + b;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT add_em(3, 4) AS v"), Value::Int(7));
    // Callable in WHERE position too.
    exec(&eng, "CREATE TABLE t_add (x INTEGER)");
    exec(&eng, "INSERT INTO t_add VALUES (1), (5), (9)");
    let result = exec(
        &eng,
        "SELECT x FROM t_add WHERE add_em(x, 1) > 5 ORDER BY x",
    );
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn strict_function_returns_null_on_null_input() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION strict_add(a int, b int) RETURNS int AS $$
         BEGIN RETURN a + b; END;
         $$ LANGUAGE plpgsql STRICT",
    );
    // PG18: strict_add(1, NULL) IS NULL => t
    assert_eq!(
        scalar(&eng, "SELECT strict_add(1, NULL) IS NULL AS is_null"),
        Value::Bool(true)
    );
    assert_eq!(scalar(&eng, "SELECT strict_add(1, 2) AS v"), Value::Int(3));
}

#[test]
fn default_arguments() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION def_add(a integer, b integer DEFAULT 5) RETURNS integer AS $$
         BEGIN RETURN a + b; END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT def_add(1) AS v"), Value::Int(6));
    assert_eq!(scalar(&eng, "SELECT def_add(1, 10) AS v"), Value::Int(11));
}

#[test]
fn named_arguments() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION named_sub(a int, b int) RETURNS int AS $$
         BEGIN RETURN a - b; END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT named_sub(b => 3, a => 10) AS v"),
        Value::Int(7)
    );
    // Mixed positional + named (PG18: positional first).
    assert_eq!(
        scalar(&eng, "SELECT named_sub(10, b => 4) AS v"),
        Value::Int(6)
    );
}

#[test]
fn pg18_bound_cursor_accepts_named_arguments() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION cursor_named(seed integer) RETURNS integer AS $$
         DECLARE
           c CURSOR (a integer, b integer) FOR SELECT a * 10 + b AS value;
           out_value integer;
         BEGIN
           OPEN c(b => seed + 2, a => seed + 1);
           FETCH c INTO out_value;
           CLOSE c;
           RETURN out_value;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT cursor_named(3) AS value"),
        Value::Int(45)
    );
}

#[test]
fn pg18_bound_cursor_tracks_position_and_found() {
    let eng = engine();
    exec(&eng, "CREATE TABLE cursor_items (id integer PRIMARY KEY)");
    exec(&eng, "INSERT INTO cursor_items VALUES (1), (2)");
    exec(
        &eng,
        "CREATE FUNCTION cursor_walk() RETURNS integer AS $$
         DECLARE
           c CURSOR FOR SELECT id FROM cursor_items ORDER BY id;
           first_value integer;
           second_value integer;
           exhausted_value integer := -1;
         BEGIN
           OPEN c;
           FETCH c INTO first_value;
           FETCH c INTO second_value;
           FETCH c INTO exhausted_value;
           IF FOUND THEN
             CLOSE c;
             RETURN -1;
           END IF;
           CLOSE c;
           IF exhausted_value IS NOT NULL THEN
             RETURN -2;
           END IF;
           RETURN first_value * 10 + second_value;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT cursor_walk() AS value"),
        Value::Int(12)
    );
}

#[test]
fn cursor_contracts_requiring_session_portals_fail_explicitly() {
    let eng = engine();
    let returns_cursor = exec_err(
        &eng,
        "CREATE FUNCTION cursor_return() RETURNS refcursor AS $$ BEGIN RETURN NULL; END; $$ LANGUAGE plpgsql",
    );
    assert!(
        returns_cursor
            .to_string()
            .contains("refcursor returns require session portal state"),
        "got: {returns_cursor}"
    );
    let cursor_parameter = exec_err(
        &eng,
        "CREATE FUNCTION cursor_input(c refcursor) RETURNS integer AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql",
    );
    assert!(
        cursor_parameter
            .to_string()
            .contains("refcursor parameters require session portal state"),
        "got: {cursor_parameter}"
    );

    exec(
        &eng,
        "CREATE FUNCTION cursor_leak() RETURNS integer AS $$
         DECLARE c CURSOR FOR SELECT 1 AS value;
         BEGIN OPEN c; RETURN 1; END;
         $$ LANGUAGE plpgsql",
    );
    let open_cursor = exec_err(&eng, "SELECT cursor_leak()");
    assert!(
        open_cursor
            .to_string()
            .contains("cursors that remain open after routine exit require session portal state"),
        "got: {open_cursor}"
    );
}

#[test]
fn out_parameters_shape_result() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION f_out(a int, OUT s int, OUT p int) AS $$
         BEGIN s := a + 1; p := a * 2; END;
         $$ LANGUAGE plpgsql",
    );
    // PG18: SELECT * FROM f_out(5) => s = 6, p = 10.
    let result = exec(&eng, "SELECT * FROM f_out(5)");
    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    assert_eq!(row.get("s"), Some(&Value::Int(6)));
    assert_eq!(row.get("p"), Some(&Value::Int(10)));
    // Scalar position folds the OUT parameters into a named record value (PG renders `(6,10)`).
    let record = scalar(&eng, "SELECT f_out(5) AS r");
    assert_eq!(
        record,
        Value::Record(vec![
            ("s".into(), Value::Int(6)),
            ("p".into(), Value::Int(10)),
        ])
    );
    // Single OUT parameter yields the bare value in scalar position.
    exec(
        &eng,
        "CREATE FUNCTION f_out1(a int, OUT doubled int) AS $$
         BEGIN doubled := a * 2; END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT f_out1(21) AS v"), Value::Int(42));
}

#[test]
fn unknown_function_error_matches_postgres_shape() {
    let eng = engine();
    let err = exec_err(&eng, "SELECT no_such_fn(1)");
    // PG18: function no_such_fn(integer) does not exist (42883)
    assert!(
        err.to_string()
            .contains("function no_such_fn(integer) does not exist"),
        "got: {err}"
    );
    assert_eq!(err.sqlstate(), Some("42883"));
}

#[test]
fn overload_resolution_by_arity() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION ovl(a int) RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql",
    );
    exec(
        &eng,
        "CREATE FUNCTION ovl(a int, b int) RETURNS int AS $$ BEGIN RETURN 2; END; $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT ovl(0) AS v"), Value::Int(1));
    assert_eq!(scalar(&eng, "SELECT ovl(0, 0) AS v"), Value::Int(2));
    // Same (name, arity) without OR REPLACE: PG18 error text.
    let err = exec_err(
        &eng,
        "CREATE FUNCTION ovl(x int) RETURNS int AS $$ BEGIN RETURN 3; END; $$ LANGUAGE plpgsql",
    );
    assert!(
        err.to_string()
            .contains("function \"ovl\" already exists with same argument types"),
        "got: {err}"
    );
}

#[test]
fn runtime_overload_resolution_prefers_float8_for_integer_input() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION runtime_pick(value double precision) RETURNS text AS $$
         BEGIN RETURN 'float8'; END;
         $$ LANGUAGE plpgsql",
    );
    exec(
        &eng,
        "CREATE FUNCTION runtime_pick(value numeric) RETURNS text AS $$
         BEGIN RETURN 'numeric'; END;
         $$ LANGUAGE plpgsql",
    );

    assert_eq!(
        scalar(&eng, "SELECT runtime_pick(1) AS selected"),
        Value::Str("float8".into())
    );
}

#[test]
fn quoted_named_arguments_preserve_identifier_case() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION quoted_named_argument(\"InputValue\" int) RETURNS int AS $$
         BEGIN RETURN \"InputValue\"; END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT quoted_named_argument(\"InputValue\" => 7) AS value",
        ),
        Value::Int(7)
    );
    let error = exec_err(
        &eng,
        "SELECT quoted_named_argument(inputvalue => 7) AS value",
    );
    assert_eq!(error.sqlstate(), Some("42883"));
}

#[test]
fn create_or_replace_replaces_body() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION rep() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT rep() AS v"), Value::Int(1));
    exec(
        &eng,
        "CREATE OR REPLACE FUNCTION rep() RETURNS int AS $$ BEGIN RETURN 2; END; $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT rep() AS v"), Value::Int(2));
    // PG18: OR REPLACE cannot change the return type.
    let err = exec_err(
        &eng,
        "CREATE OR REPLACE FUNCTION rep() RETURNS text AS $$ BEGIN RETURN 'x'; END; $$ LANGUAGE plpgsql",
    );
    assert!(
        err.to_string()
            .contains("cannot change return type of existing function"),
        "got: {err}"
    );
    // PG18: OR REPLACE cannot change the routine kind.
    let err = exec_err(
        &eng,
        "CREATE OR REPLACE PROCEDURE rep() AS $$ BEGIN NULL; END; $$ LANGUAGE plpgsql",
    );
    assert!(
        err.to_string().contains("cannot change routine kind"),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------
// Set-returning functions
// ---------------------------------------------------------------------
