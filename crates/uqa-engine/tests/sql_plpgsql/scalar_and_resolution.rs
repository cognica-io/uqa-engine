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
    // PG17: strict_add(1, NULL) IS NULL => t
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
    // Mixed positional + named (PG17: positional first).
    assert_eq!(
        scalar(&eng, "SELECT named_sub(10, b => 4) AS v"),
        Value::Int(6)
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
    // PG17: SELECT * FROM f_out(5) => s = 6, p = 10.
    let result = exec(&eng, "SELECT * FROM f_out(5)");
    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    assert_eq!(row.get("s"), Some(&Value::Int(6)));
    assert_eq!(row.get("p"), Some(&Value::Int(10)));
    // Scalar position folds the OUT parameters into a record value
    // (PG renders `(6,10)`; the engine represents it as a map).
    let record = scalar(&eng, "SELECT f_out(5) AS r");
    match record {
        Value::Map(map) => {
            assert_eq!(map.get("s"), Some(&Value::Int(6)));
            assert_eq!(map.get("p"), Some(&Value::Int(10)));
        }
        other => panic!("expected record map, got {other:?}"),
    }
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
    // PG17: function no_such_fn(integer) does not exist (42883)
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
    // Same (name, arity) without OR REPLACE: PG17 error text.
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
    // PG17: OR REPLACE cannot change the return type.
    let err = exec_err(
        &eng,
        "CREATE OR REPLACE FUNCTION rep() RETURNS text AS $$ BEGIN RETURN 'x'; END; $$ LANGUAGE plpgsql",
    );
    assert!(
        err.to_string()
            .contains("cannot change return type of existing function"),
        "got: {err}"
    );
    // PG17: OR REPLACE cannot change the routine kind.
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
