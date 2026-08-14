//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

// ---------------------------------------------------------------------
// Misc semantics
// ---------------------------------------------------------------------

#[test]
fn nested_blocks_and_exception_recovery() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION nested() RETURNS text AS $$
         DECLARE msg text := '';
         BEGIN
           msg := msg || 'a';
           BEGIN
             msg := msg || 'b';
             RAISE EXCEPTION 'oops';
           EXCEPTION
             WHEN OTHERS THEN msg := msg || 'c';
           END;
           msg := msg || 'd';
           RETURN msg;
         END;
         $$ LANGUAGE plpgsql",
    );
    // PG18: abcd - the inner handler recovers, outer code continues.
    assert_eq!(
        scalar(&eng, "SELECT nested() AS v"),
        Value::Str("abcd".into())
    );
}

#[test]
fn variable_defaults_and_declarations() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION decls() RETURNS int AS $$
         DECLARE
           a int := 10;
           b int DEFAULT 20;
           c int;
         BEGIN
           -- c defaults to NULL (PG18).
           IF c IS NULL THEN
             RETURN a + b;
           END IF;
           RETURN -1;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT decls() AS v"), Value::Int(30));
    // Defaults may reference parameters (evaluated at entry).
    exec(
        &eng,
        "CREATE FUNCTION decl_param(x int) RETURNS int AS $$
         DECLARE y int := x * 2;
         BEGIN RETURN y; END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT decl_param(8) AS v"), Value::Int(16));
}

#[test]
fn scalar_subquery_expressions_inside_body() {
    let eng = engine();
    exec(&eng, "CREATE TABLE sub_t (v INTEGER)");
    exec(&eng, "INSERT INTO sub_t VALUES (1), (2), (3)");
    exec(
        &eng,
        "CREATE FUNCTION count_plus(extra int) RETURNS int AS $$
         DECLARE n int;
         BEGIN
           n := (SELECT count(*) FROM sub_t) + extra;
           RETURN n;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT count_plus(10) AS v"), Value::Int(13));
}

#[test]
fn return_query_execute_dynamic() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION rqe(n int) RETURNS SETOF int AS $$
         BEGIN
           RETURN QUERY EXECUTE 'SELECT g * $1 FROM generate_series(1, 2) AS g' USING n;
         END;
         $$ LANGUAGE plpgsql",
    );
    // PG18: 5, 10
    let result = exec(&eng, "SELECT * FROM rqe(5)");
    let values: Vec<Value> = result
        .rows
        .iter()
        .map(|row| row.get("rqe").cloned().unwrap())
        .collect();
    assert_eq!(values, vec![Value::Int(5), Value::Int(10)]);
}

#[test]
fn positional_dollar_references_in_body() {
    let eng = engine();
    // Unnamed parameters are only addressable as $n (PG18: 304).
    exec(
        &eng,
        "CREATE FUNCTION dollar_ref(int, int) RETURNS int AS $$
         BEGIN
           RETURN $1 * 100 + $2;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT dollar_ref(3, 4) AS v"),
        Value::Int(304)
    );
}

#[test]
fn call_procedure_inside_function_body() {
    let eng = engine();
    exec(&eng, "CREATE TABLE bumps (v INTEGER)");
    exec(
        &eng,
        "CREATE PROCEDURE bump(x int) AS $$ BEGIN INSERT INTO bumps VALUES (x); END $$ LANGUAGE plpgsql",
    );
    exec(
        &eng,
        "CREATE FUNCTION call_proc() RETURNS int AS $$
         BEGIN
           CALL bump(7);
           RETURN (SELECT count(*) FROM bumps);
         END;
         $$ LANGUAGE plpgsql",
    );
    // PG18: 1
    assert_eq!(scalar(&eng, "SELECT call_proc() AS v"), Value::Int(1));
}

#[test]
fn named_arguments_and_aliases_in_from() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION gen5(n int) RETURNS SETOF int AS $$
           SELECT g FROM generate_series(1, n) AS g
         $$ LANGUAGE sql",
    );
    // Named argument in FROM position (PG18: 1, 2).
    let result = exec(&eng, "SELECT * FROM gen5(n => 2)");
    assert_eq!(result.rows.len(), 2);
    // Column alias list renames the output column (PG18: val = 2).
    assert_eq!(
        scalar(
            &eng,
            "SELECT val FROM gen5(2) AS t(val) ORDER BY val DESC LIMIT 1"
        ),
        Value::Int(2)
    );
}

#[test]
fn default_arguments_make_overloads_ambiguous() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION amb(a int) RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql",
    );
    exec(
        &eng,
        "CREATE FUNCTION amb(a int, b int DEFAULT 1) RETURNS int AS $$ SELECT 2 $$ LANGUAGE sql",
    );
    // PG18: function amb(integer) is not unique.
    let err = exec_err(&eng, "SELECT amb(9) AS v");
    assert!(
        err.to_string()
            .contains("function amb(integer) is not unique"),
        "got: {err}"
    );
    // The two-argument call is unambiguous.
    assert_eq!(scalar(&eng, "SELECT amb(9, 9) AS v"), Value::Int(2));
}

#[test]
fn percent_type_declarations_resolve_and_enforce_the_referenced_column_type() {
    let eng = engine();
    exec(&eng, "CREATE TABLE typed (id SMALLINT, name TEXT)");
    exec(&eng, "INSERT INTO typed VALUES (7, 'seven')");
    exec(
        &eng,
        "CREATE FUNCTION typed_lookup(which int) RETURNS text AS $$
         DECLARE v typed.name%TYPE;
         BEGIN
           SELECT name INTO v FROM typed WHERE id = which;
           RETURN v;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT typed_lookup(7) AS v"),
        Value::Str("seven".into())
    );

    exec(
        &eng,
        "CREATE FUNCTION typed_overflow() RETURNS typed.id%TYPE AS $$
         DECLARE v typed.id%TYPE;
         BEGIN
           v := 40000;
           RETURN v;
         END;
         $$ LANGUAGE plpgsql",
    );
    let error = exec_err(&eng, "SELECT typed_overflow() AS v");
    assert_eq!(error.sqlstate(), Some("22003"));
    assert!(error.to_string().contains("smallint out of range"));
}

#[test]
fn function_usable_inside_view_and_aggregate() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION double_it(v int) RETURNS int AS $$
         BEGIN RETURN v * 2; END;
         $$ LANGUAGE plpgsql",
    );
    exec(&eng, "CREATE TABLE agg_t (v INTEGER)");
    exec(&eng, "INSERT INTO agg_t VALUES (1), (2), (3)");
    assert_eq!(
        scalar(&eng, "SELECT sum(double_it(v)) AS s FROM agg_t"),
        Value::Int(12)
    );
    exec(
        &eng,
        "CREATE VIEW doubled AS SELECT double_it(v) AS dv FROM agg_t",
    );
    let result = exec(&eng, "SELECT dv FROM doubled ORDER BY dv");
    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.rows[2].get("dv"), Some(&Value::Int(6)));
}
