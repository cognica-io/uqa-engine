//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! User-defined function and procedure coverage: `CREATE FUNCTION` /
//! `CREATE PROCEDURE` / `DO` / `CALL` with `LANGUAGE plpgsql` and
//! `LANGUAGE sql`. Expected outcomes were verified against
//! `PostgreSQL` 17.7 unless a comment states a documented divergence.

use uqa_core::Value;
use uqa_engine::{Engine, SQLResult};
use uqa_sql::SQLError;

fn exec(engine: &Engine, sql: &str) -> SQLResult {
    match engine.sql(sql, &[]) {
        Ok(result) => result,
        Err(e) => panic!("SQL failed: {e}\n  sql: {sql}"),
    }
}

fn exec_err(engine: &Engine, sql: &str) -> SQLError {
    match engine.sql(sql, &[]) {
        Ok(_) => panic!("expected error for: {sql}"),
        Err(e) => e,
    }
}

fn scalar(engine: &Engine, sql: &str) -> Value {
    let result = exec(engine, sql);
    let row = result.rows.first().unwrap_or_else(|| {
        panic!("no rows for: {sql}");
    });
    let column = result.columns.first().expect("no columns");
    row.get(column).cloned().unwrap_or(Value::Null)
}

fn engine() -> Engine {
    Engine::new()
}

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

#[test]
fn returns_setof_with_return_next() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION gen(n int) RETURNS SETOF integer AS $$
         BEGIN
           FOR i IN 1..n LOOP
             RETURN NEXT i * 10;
           END LOOP;
           RETURN;
         END;
         $$ LANGUAGE plpgsql",
    );
    let result = exec(&eng, "SELECT * FROM gen(3)");
    let values: Vec<Value> = result
        .rows
        .iter()
        .map(|row| row.get("gen").cloned().unwrap())
        .collect();
    assert_eq!(values, vec![Value::Int(10), Value::Int(20), Value::Int(30)]);
    // PG17: reaching the end of a SETOF function without RETURN is
    // fine; the accumulated set is returned.
    exec(
        &eng,
        "CREATE FUNCTION gen2() RETURNS SETOF int AS $$
         BEGIN RETURN NEXT 1; RETURN NEXT 2; END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(exec(&eng, "SELECT * FROM gen2()").rows.len(), 2);
}

#[test]
fn returns_table_with_return_query() {
    let eng = engine();
    exec(&eng, "CREATE TABLE items (id INTEGER, label TEXT)");
    exec(
        &eng,
        "INSERT INTO items VALUES (1, 'one'), (2, 'two'), (3, 'three')",
    );
    exec(
        &eng,
        "CREATE FUNCTION list_items(min_id int) RETURNS TABLE(id int, label text) AS $$
         BEGIN
           RETURN QUERY SELECT items.id, items.label FROM items
                        WHERE items.id >= min_id ORDER BY items.id;
         END;
         $$ LANGUAGE plpgsql",
    );
    let result = exec(&eng, "SELECT * FROM list_items(2)");
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0].get("id"), Some(&Value::Int(2)));
    assert_eq!(result.rows[0].get("label"), Some(&Value::Str("two".into())));
    // RETURN NEXT with TABLE columns assigns the column variables.
    exec(
        &eng,
        "CREATE FUNCTION tbl_next(n int) RETURNS TABLE(x int, y text) AS $$
         BEGIN
           x := n; y := 'a'; RETURN NEXT;
           x := n + 1; y := 'b'; RETURN NEXT;
         END;
         $$ LANGUAGE plpgsql",
    );
    let result = exec(&eng, "SELECT * FROM tbl_next(7)");
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[1].get("x"), Some(&Value::Int(8)));
    assert_eq!(result.rows[1].get("y"), Some(&Value::Str("b".into())));
}

#[test]
fn set_valued_function_rejected_in_scalar_context() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION setf() RETURNS SETOF int AS $$
         BEGIN RETURN NEXT 1; END;
         $$ LANGUAGE plpgsql",
    );
    // Documented divergence: PG17 expands SRFs in the select list;
    // the engine rejects them outside FROM with PG's wording for
    // contexts that cannot accept a set.
    let err = exec_err(&eng, "SELECT abs(setf()) AS v");
    assert!(
        err.to_string()
            .contains("set-valued function called in context that cannot accept a set"),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------
// Control flow
// ---------------------------------------------------------------------

#[test]
fn if_elsif_else_and_case() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION classify(n int) RETURNS text AS $$
         BEGIN
           IF n < 0 THEN
             RETURN 'negative';
           ELSIF n = 0 THEN
             RETURN 'zero';
           ELSE
             RETURN 'positive';
           END IF;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT classify(-5) AS v"),
        Value::Str("negative".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT classify(0) AS v"),
        Value::Str("zero".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT classify(9) AS v"),
        Value::Str("positive".into())
    );
    // Simple CASE (WHEN lists) and searched CASE statements.
    exec(
        &eng,
        "CREATE FUNCTION case_kind(n int) RETURNS text AS $$
         DECLARE r text;
         BEGIN
           CASE n
             WHEN 1, 2 THEN r := 'small';
             WHEN 3 THEN r := 'three';
             ELSE r := 'big';
           END CASE;
           CASE
             WHEN n > 10 THEN r := r || '+';
             ELSE r := r || '-';
           END CASE;
           RETURN r;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT case_kind(2) AS v"),
        Value::Str("small-".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT case_kind(99) AS v"),
        Value::Str("big+".into())
    );
    // PG17: CASE with no matching arm and no ELSE => "case not found".
    exec(
        &eng,
        "CREATE FUNCTION case_miss(n int) RETURNS int AS $$
         BEGIN
           CASE n WHEN 1 THEN RETURN 1; END CASE;
           RETURN 0;
         END;
         $$ LANGUAGE plpgsql",
    );
    let err = exec_err(&eng, "SELECT case_miss(5) AS v");
    assert!(err.to_string().contains("case not found"), "got: {err}");
    assert_eq!(err.sqlstate(), Some("20000"));
}

#[test]
fn loops_with_exit_and_continue() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION loop_sum(n int) RETURNS int AS $$
         DECLARE total int := 0;
                 i int := 0;
         BEGIN
           LOOP
             i := i + 1;
             EXIT WHEN i > n;
             CONTINUE WHEN i % 2 = 0;
             total := total + i;
           END LOOP;
           RETURN total;
         END;
         $$ LANGUAGE plpgsql",
    );
    // 1 + 3 + 5 + 7 + 9 = 25 (verified against PG17)
    assert_eq!(scalar(&eng, "SELECT loop_sum(10) AS v"), Value::Int(25));

    exec(
        &eng,
        "CREATE FUNCTION while_sum(n int) RETURNS int AS $$
         DECLARE total int := 0; i int := 1;
         BEGIN
           WHILE i <= n LOOP
             total := total + i;
             i := i + 1;
           END LOOP;
           RETURN total;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT while_sum(4) AS v"), Value::Int(10));

    // Integer FOR loops: forward, REVERSE, and BY step.
    exec(
        &eng,
        "CREATE FUNCTION for_text() RETURNS text AS $$
         DECLARE acc text := '';
         BEGIN
           FOR i IN 1..3 LOOP acc := acc || i; END LOOP;
           acc := acc || ':';
           FOR i IN REVERSE 3..1 LOOP acc := acc || i; END LOOP;
           acc := acc || ':';
           FOR i IN 1..7 BY 3 LOOP acc := acc || i; END LOOP;
           acc := acc || ':';
           -- PG17: REVERSE 1..3 iterates zero times.
           FOR i IN REVERSE 1..3 LOOP acc := acc || i; END LOOP;
           RETURN acc;
         END;
         $$ LANGUAGE plpgsql",
    );
    // Verified against PG17: 123:321:147:
    assert_eq!(
        scalar(&eng, "SELECT for_text() AS v"),
        Value::Str("123:321:147:".into())
    );
    // PG17: BY 0 => "BY value of FOR loop must be greater than zero".
    exec(
        &eng,
        "CREATE FUNCTION for_zero() RETURNS int AS $$
         BEGIN
           FOR i IN 1..3 BY 0 LOOP RETURN i; END LOOP;
           RETURN 0;
         END;
         $$ LANGUAGE plpgsql",
    );
    let err = exec_err(&eng, "SELECT for_zero() AS v");
    assert!(
        err.to_string()
            .contains("BY value of FOR loop must be greater than zero"),
        "got: {err}"
    );
}

#[test]
fn labeled_loops_and_blocks() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION labeled() RETURNS int AS $$
         DECLARE total int := 0;
         BEGIN
           <<outer_loop>>
           FOR i IN 1..5 LOOP
             FOR j IN 1..5 LOOP
               total := total + 1;
               CONTINUE outer_loop WHEN j = 2;
               EXIT outer_loop WHEN total > 50;
             END LOOP;
           END LOOP;
           RETURN total;
         END;
         $$ LANGUAGE plpgsql",
    );
    // Each outer iteration runs j=1,2 then continues: 5 * 2 = 10.
    assert_eq!(scalar(&eng, "SELECT labeled() AS v"), Value::Int(10));
    // EXIT with a block label leaves the block.
    exec(
        &eng,
        "CREATE FUNCTION block_exit() RETURNS int AS $$
         BEGIN
           <<blk>>
           BEGIN
             EXIT blk;
             RETURN 1;
           END;
           RETURN 2;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT block_exit() AS v"), Value::Int(2));
}

#[test]
fn for_over_query_and_record_fields() {
    let eng = engine();
    exec(&eng, "CREATE TABLE nums (v INTEGER)");
    exec(&eng, "INSERT INTO nums VALUES (1), (2), (3), (4)");
    exec(
        &eng,
        "CREATE FUNCTION sum_evens() RETURNS int AS $$
         DECLARE rec RECORD;
                 total int := 0;
         BEGIN
           FOR rec IN SELECT v FROM nums ORDER BY v LOOP
             CONTINUE WHEN rec.v % 2 = 1;
             total := total + rec.v;
           END LOOP;
           IF FOUND THEN
             total := total + 100;
           END IF;
           RETURN total;
         END;
         $$ LANGUAGE plpgsql",
    );
    // 2 + 4 + 100 (FOUND set by the FOR loop) = 106
    assert_eq!(scalar(&eng, "SELECT sum_evens() AS v"), Value::Int(106));
}

// ---------------------------------------------------------------------
// SELECT INTO, FOUND, GET DIAGNOSTICS
// ---------------------------------------------------------------------

#[test]
fn select_into_strict_errors() {
    let eng = engine();
    exec(&eng, "CREATE TABLE si (v INTEGER)");
    exec(&eng, "INSERT INTO si VALUES (1), (2)");
    exec(
        &eng,
        "CREATE FUNCTION pick(which int) RETURNS int AS $$
         DECLARE x int;
         BEGIN
           IF which = 0 THEN
             SELECT v INTO STRICT x FROM si WHERE v > 100;
           ELSIF which = 1 THEN
             SELECT v INTO STRICT x FROM si WHERE v = 1;
           ELSE
             SELECT v INTO STRICT x FROM si;
           END IF;
           RETURN x;
         END;
         $$ LANGUAGE plpgsql",
    );
    // PG17: "query returned no rows" (P0002)
    let err = exec_err(&eng, "SELECT pick(0) AS v");
    assert!(
        err.to_string().contains("query returned no rows"),
        "got: {err}"
    );
    assert_eq!(err.sqlstate(), Some("P0002"));
    assert_eq!(scalar(&eng, "SELECT pick(1) AS v"), Value::Int(1));
    // PG17: "query returned more than one row" (P0003)
    let err = exec_err(&eng, "SELECT pick(2) AS v");
    assert!(
        err.to_string().contains("query returned more than one row"),
        "got: {err}"
    );
    assert_eq!(err.sqlstate(), Some("P0003"));
    // no_data_found / too_many_rows handlers catch them.
    exec(
        &eng,
        "CREATE FUNCTION pick_safe(which int) RETURNS int AS $$
         DECLARE x int;
         BEGIN
           IF which = 0 THEN
             SELECT v INTO STRICT x FROM si WHERE v > 100;
           ELSE
             SELECT v INTO STRICT x FROM si;
           END IF;
           RETURN x;
         EXCEPTION
           WHEN no_data_found THEN RETURN -1;
           WHEN too_many_rows THEN RETURN -2;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT pick_safe(0) AS v"), Value::Int(-1));
    assert_eq!(scalar(&eng, "SELECT pick_safe(1) AS v"), Value::Int(-2));
}

#[test]
fn select_into_non_strict_and_found() {
    let eng = engine();
    exec(&eng, "CREATE TABLE sf (v INTEGER)");
    exec(&eng, "INSERT INTO sf VALUES (7)");
    exec(
        &eng,
        "CREATE FUNCTION probe(which int) RETURNS text AS $$
         DECLARE x int;
         BEGIN
           IF which = 0 THEN
             SELECT v INTO x FROM sf WHERE v > 100;
           ELSE
             SELECT v INTO x FROM sf;
           END IF;
           IF FOUND THEN
             RETURN 'found:' || x;
           ELSE
             -- PG17: non-strict INTO with no rows leaves NULLs.
             RETURN 'missing:' || COALESCE(x, -1);
           END IF;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT probe(1) AS v"),
        Value::Str("found:7".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT probe(0) AS v"),
        Value::Str("missing:-1".into())
    );
}

#[test]
fn found_after_dml_and_get_diagnostics_row_count() {
    let eng = engine();
    exec(&eng, "CREATE TABLE dml (v INTEGER)");
    exec(&eng, "INSERT INTO dml VALUES (1), (2), (3)");
    exec(
        &eng,
        "CREATE FUNCTION touch(threshold int) RETURNS int AS $$
         DECLARE n int;
         BEGIN
           UPDATE dml SET v = v + 10 WHERE v >= threshold;
           IF NOT FOUND THEN
             RETURN -1;
           END IF;
           GET DIAGNOSTICS n = ROW_COUNT;
           RETURN n;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT touch(2) AS v"), Value::Int(2));
    assert_eq!(scalar(&eng, "SELECT touch(1000) AS v"), Value::Int(-1));
}

#[test]
fn perform_sets_found() {
    let eng = engine();
    exec(&eng, "CREATE TABLE pf (v INTEGER)");
    exec(&eng, "INSERT INTO pf VALUES (1)");
    exec(
        &eng,
        "CREATE FUNCTION perform_probe(needle int) RETURNS bool AS $$
         BEGIN
           PERFORM v FROM pf WHERE v = needle;
           RETURN FOUND;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT perform_probe(1) AS v"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(&eng, "SELECT perform_probe(2) AS v"),
        Value::Bool(false)
    );
}

// ---------------------------------------------------------------------
// RAISE and exception handling
// ---------------------------------------------------------------------

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

#[test]
fn execute_dynamic_sql_with_using_and_into() {
    let eng = engine();
    exec(&eng, "CREATE TABLE dyn (v INTEGER)");
    exec(&eng, "INSERT INTO dyn VALUES (5)");
    exec(
        &eng,
        "CREATE FUNCTION dyn_add(a int, b int) RETURNS int AS $$
         DECLARE result int;
         BEGIN
           EXECUTE 'SELECT $1 + $2' INTO result USING a, b;
           RETURN result;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT dyn_add(2, 3) AS v"), Value::Int(5));
    // Dynamic DML plus GET DIAGNOSTICS.
    exec(
        &eng,
        "CREATE FUNCTION dyn_dml() RETURNS int AS $$
         DECLARE n int;
         BEGIN
           EXECUTE 'INSERT INTO dyn VALUES (6), (7)';
           GET DIAGNOSTICS n = ROW_COUNT;
           RETURN n;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT dyn_dml() AS v"), Value::Int(2));
    // Dynamic query built from strings, INTO STRICT.
    exec(
        &eng,
        "CREATE FUNCTION dyn_query(needle int) RETURNS int AS $$
         DECLARE found_v int;
         BEGIN
           EXECUTE 'SELECT v FROM dyn WHERE v = ' || needle INTO STRICT found_v;
           RETURN found_v;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT dyn_query(6) AS v"), Value::Int(6));
    let err = exec_err(&eng, "SELECT dyn_query(999) AS v");
    assert!(
        err.to_string().contains("query returned no rows"),
        "got: {err}"
    );
    // PG17: EXECUTE of a NULL query string fails.
    let err = exec_err(&eng, "DO $$ DECLARE q text; BEGIN EXECUTE q; END $$");
    assert!(
        err.to_string()
            .contains("query string argument of EXECUTE is null"),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------
// Recursion
// ---------------------------------------------------------------------

#[test]
fn recursive_factorial() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION fact(n int) RETURNS int AS $$
         BEGIN
           IF n <= 1 THEN
             RETURN 1;
           END IF;
           RETURN n * fact(n - 1);
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT fact(10) AS v"), Value::Int(3_628_800));
}

#[test]
fn infinite_recursion_hits_depth_limit() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION inf_rec(n int) RETURNS int AS $$
         BEGIN RETURN inf_rec(n + 1); END;
         $$ LANGUAGE plpgsql",
    );
    // PG17: stack depth limit exceeded. The default guard (frame cap
    // plus native stack budget) must fire before the thread stack is
    // exhausted.
    let err = exec_err(&eng, "SELECT inf_rec(0) AS v");
    assert!(
        err.to_string().contains("stack depth limit exceeded"),
        "got: {err}"
    );
    assert_eq!(err.sqlstate(), Some("54001"));
    // A tightened limit fires earlier but with the same shape.
    eng.set_sql_function_depth_limit(4);
    let err = exec_err(&eng, "SELECT inf_rec(0) AS v");
    assert!(
        err.to_string().contains("stack depth limit exceeded"),
        "got: {err}"
    );
    // Legitimate recursion below the limit still works afterwards.
    eng.set_sql_function_depth_limit(128);
    exec(
        &eng,
        "CREATE FUNCTION fib(n int) RETURNS int AS $$
         BEGIN
           IF n < 2 THEN RETURN n; END IF;
           RETURN fib(n - 1) + fib(n - 2);
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(scalar(&eng, "SELECT fib(10) AS v"), Value::Int(55));
}

// ---------------------------------------------------------------------
// Procedures, CALL, DO
// ---------------------------------------------------------------------

#[test]
fn procedure_with_inout_via_call() {
    let eng = engine();
    exec(
        &eng,
        "CREATE PROCEDURE p_inout(INOUT x int, IN y int) AS $$
         BEGIN x := x + y; END;
         $$ LANGUAGE plpgsql",
    );
    // PG17: CALL returns a result row named after the INOUT param.
    let result = exec(&eng, "CALL p_inout(10, 5)");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("x"), Some(&Value::Int(15)));
    // Procedure with OUT parameter: PG14+ requires a placeholder
    // argument in CALL.
    exec(
        &eng,
        "CREATE PROCEDURE p_out(IN a int, OUT doubled int) AS $$
         BEGIN doubled := a * 2; END;
         $$ LANGUAGE plpgsql",
    );
    let result = exec(&eng, "CALL p_out(21, NULL)");
    assert_eq!(result.rows[0].get("doubled"), Some(&Value::Int(42)));
    // PG17 error shapes for kind confusion.
    let err = exec_err(&eng, "SELECT p_inout(1, 2) AS v");
    assert!(err.to_string().contains("is a procedure"), "got: {err}");
    exec(
        &eng,
        "CREATE FUNCTION plainf(x int) RETURNS int AS $$ BEGIN RETURN x; END; $$ LANGUAGE plpgsql",
    );
    let err = exec_err(&eng, "CALL plainf(1)");
    assert!(err.to_string().contains("is not a procedure"), "got: {err}");
    // PG17: CALL with the wrong arity names the procedure.
    let err = exec_err(&eng, "CALL p_inout(1)");
    assert!(
        err.to_string()
            .contains("procedure p_inout(integer) does not exist"),
        "got: {err}"
    );
}

#[test]
fn procedure_mutating_table_state() {
    let eng = engine();
    exec(&eng, "CREATE TABLE audit_log (msg TEXT)");
    exec(
        &eng,
        "CREATE PROCEDURE log_msg(m text) AS $$
         BEGIN
           INSERT INTO audit_log VALUES (m);
         END;
         $$ LANGUAGE plpgsql",
    );
    exec(&eng, "CALL log_msg('hello')");
    exec(&eng, "CALL log_msg('world')");
    let result = exec(&eng, "SELECT count(*) AS n FROM audit_log");
    assert_eq!(result.rows[0].get("n"), Some(&Value::Int(2)));
}

#[test]
fn do_block_side_effects() {
    let eng = engine();
    exec(&eng, "CREATE TABLE do_target (v INTEGER)");
    exec(
        &eng,
        "DO $$
         BEGIN
           FOR i IN 1..3 LOOP
             INSERT INTO do_target VALUES (i * 100);
           END LOOP;
         END
         $$",
    );
    let result = exec(&eng, "SELECT count(*) AS n, max(v) AS m FROM do_target");
    assert_eq!(result.rows[0].get("n"), Some(&Value::Int(3)));
    assert_eq!(result.rows[0].get("m"), Some(&Value::Int(300)));
    // DO LANGUAGE plpgsql spelled explicitly.
    exec(
        &eng,
        "DO LANGUAGE plpgsql $$ BEGIN INSERT INTO do_target VALUES (1); END $$",
    );
    // PG17: only procedural languages work in DO.
    let err = exec_err(&eng, "DO LANGUAGE sql $$ SELECT 1 $$");
    assert!(
        err.to_string().contains("language \"sql\" does not exist"),
        "got: {err}"
    );
}

// ---------------------------------------------------------------------
// LANGUAGE sql functions
// ---------------------------------------------------------------------

#[test]
fn sql_language_scalar_and_setof() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION sql_add(a integer, b integer) RETURNS integer AS $$
           SELECT a + b
         $$ LANGUAGE sql",
    );
    assert_eq!(scalar(&eng, "SELECT sql_add(20, 22) AS v"), Value::Int(42));
    // Positional $n references work too.
    exec(
        &eng,
        "CREATE FUNCTION sql_pos(integer, integer) RETURNS integer AS $$
           SELECT $1 * $2
         $$ LANGUAGE sql",
    );
    assert_eq!(scalar(&eng, "SELECT sql_pos(6, 7) AS v"), Value::Int(42));
    // SETOF: every row of the last statement.
    exec(&eng, "CREATE TABLE sql_rows (v INTEGER)");
    exec(&eng, "INSERT INTO sql_rows VALUES (1), (2), (3)");
    exec(
        &eng,
        "CREATE FUNCTION above(threshold int) RETURNS SETOF integer AS $$
           SELECT v FROM sql_rows WHERE v > threshold ORDER BY v
         $$ LANGUAGE sql",
    );
    let result = exec(&eng, "SELECT * FROM above(1)");
    assert_eq!(result.rows.len(), 2);
    // An empty SETOF result produces zero rows in FROM (PG17).
    assert_eq!(
        scalar(&eng, "SELECT count(*) AS n FROM above(100)"),
        Value::Int(0)
    );
    // Multi-statement body: the last statement's result wins.
    exec(&eng, "CREATE TABLE sql_log (v INTEGER)");
    exec(
        &eng,
        "CREATE FUNCTION log_and_count(x int) RETURNS bigint AS $$
           INSERT INTO sql_log VALUES (x);
           SELECT count(*) FROM sql_log
         $$ LANGUAGE sql",
    );
    assert_eq!(scalar(&eng, "SELECT log_and_count(1) AS v"), Value::Int(1));
    assert_eq!(scalar(&eng, "SELECT log_and_count(2) AS v"), Value::Int(2));
}

#[test]
fn sql_language_standard_body() {
    let eng = engine();
    // PG14+ SQL-standard body (no dollar quoting): RETURN expr.
    exec(
        &eng,
        "CREATE FUNCTION std_body(a int) RETURNS int RETURN a * 3",
    );
    assert_eq!(scalar(&eng, "SELECT std_body(5) AS v"), Value::Int(15));
    // BEGIN ATOMIC form.
    exec(
        &eng,
        "CREATE FUNCTION std_atomic(a int) RETURNS int
         BEGIN ATOMIC
           SELECT a + 100;
         END",
    );
    assert_eq!(scalar(&eng, "SELECT std_atomic(5) AS v"), Value::Int(105));
}

#[test]
fn sql_language_table_function() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION pairs(n int) RETURNS TABLE(x int, y int) AS $$
           SELECT g, g * n FROM generate_series(1, 3) AS g
         $$ LANGUAGE sql",
    );
    let result = exec(&eng, "SELECT * FROM pairs(10) ORDER BY x");
    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.rows[2].get("x"), Some(&Value::Int(3)));
    assert_eq!(result.rows[2].get("y"), Some(&Value::Int(30)));
}

// ---------------------------------------------------------------------
// DDL: DROP FUNCTION, persistence
// ---------------------------------------------------------------------

#[test]
fn drop_function_variants() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION d1(a int) RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql",
    );
    exec(
        &eng,
        "CREATE FUNCTION d1(a int, b int) RETURNS int AS $$ BEGIN RETURN 2; END; $$ LANGUAGE plpgsql",
    );
    // PG17: bare name with two overloads is ambiguous.
    let err = exec_err(&eng, "DROP FUNCTION d1");
    assert!(
        err.to_string()
            .contains("function name \"d1\" is not unique"),
        "got: {err}"
    );
    // Dropping by signature works.
    exec(&eng, "DROP FUNCTION d1(int)");
    assert_eq!(scalar(&eng, "SELECT d1(1, 2) AS v"), Value::Int(2));
    // Now the bare form resolves.
    exec(&eng, "DROP FUNCTION d1");
    let err = exec_err(&eng, "SELECT d1(1, 2) AS v");
    assert!(err.to_string().contains("does not exist"), "got: {err}");
    // PG17: DROP FUNCTION of an unknown bare name.
    let err = exec_err(&eng, "DROP FUNCTION never_existed");
    assert!(
        err.to_string()
            .contains("could not find a function named \"never_existed\""),
        "got: {err}"
    );
    // IF EXISTS produces a notice, not an error (PG17).
    exec(&eng, "DROP FUNCTION IF EXISTS never_existed");
    let notices = eng.take_sql_notices();
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0].1.contains("does not exist, skipping"),
        "got: {notices:?}"
    );
    // DROP PROCEDURE mirrors the behavior.
    exec(
        &eng,
        "CREATE PROCEDURE dp() AS $$ BEGIN NULL; END; $$ LANGUAGE plpgsql",
    );
    exec(&eng, "DROP PROCEDURE dp");
    let err = exec_err(&eng, "CALL dp()");
    assert!(err.to_string().contains("does not exist"), "got: {err}");
}

#[test]
fn functions_persist_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("plpgsql_persist.db");
    {
        let eng = Engine::open(&db).unwrap();
        exec(
            &eng,
            "CREATE FUNCTION persisted_add(a int, b int DEFAULT 5) RETURNS int AS $$
             BEGIN RETURN a + b; END;
             $$ LANGUAGE plpgsql STRICT",
        );
        exec(
            &eng,
            "CREATE FUNCTION persisted_sql(n int) RETURNS SETOF int AS $$
               SELECT g * n FROM generate_series(1, 2) AS g
             $$ LANGUAGE sql",
        );
        exec(
            &eng,
            "CREATE PROCEDURE persisted_proc(INOUT x int) AS $$
             BEGIN x := x * 10; END;
             $$ LANGUAGE plpgsql",
        );
        assert_eq!(scalar(&eng, "SELECT persisted_add(1) AS v"), Value::Int(6));
    }
    {
        let eng = Engine::open(&db).unwrap();
        assert_eq!(scalar(&eng, "SELECT persisted_add(1) AS v"), Value::Int(6));
        assert_eq!(
            scalar(&eng, "SELECT persisted_add(1, NULL) IS NULL AS v"),
            Value::Bool(true)
        );
        let rows = exec(&eng, "SELECT * FROM persisted_sql(3)");
        assert_eq!(rows.rows.len(), 2);
        let result = exec(&eng, "CALL persisted_proc(4)");
        assert_eq!(result.rows[0].get("x"), Some(&Value::Int(40)));
        // DROP persists as well.
        exec(&eng, "DROP FUNCTION persisted_add(int, int)");
    }
    {
        let eng = Engine::open(&db).unwrap();
        let err = exec_err(&eng, "SELECT persisted_add(1) AS v");
        assert!(err.to_string().contains("does not exist"), "got: {err}");
        // The other function survived.
        assert_eq!(exec(&eng, "SELECT * FROM persisted_sql(3)").rows.len(), 2);
    }
}

// ---------------------------------------------------------------------
// Catalog exposure
// ---------------------------------------------------------------------

#[test]
fn functions_visible_in_catalogs() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION cat_fn(a int) RETURNS SETOF int AS $$
         BEGIN RETURN NEXT a; END;
         $$ LANGUAGE plpgsql STRICT",
    );
    exec(
        &eng,
        "CREATE PROCEDURE cat_proc() AS $$ BEGIN NULL; END; $$ LANGUAGE plpgsql",
    );
    let result = exec(
        &eng,
        "SELECT proname, prokind, proisstrict, proretset, pronargs
         FROM pg_catalog.pg_proc WHERE proname = 'cat_fn'",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("prokind"), Some(&Value::Str("f".into())));
    assert_eq!(result.rows[0].get("proisstrict"), Some(&Value::Bool(true)));
    assert_eq!(result.rows[0].get("proretset"), Some(&Value::Bool(true)));
    assert_eq!(result.rows[0].get("pronargs"), Some(&Value::Int(1)));
    let result = exec(
        &eng,
        "SELECT routine_name, routine_type, external_language
         FROM information_schema.routines WHERE routine_name = 'cat_proc'",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(
        result.rows[0].get("routine_type"),
        Some(&Value::Str("PROCEDURE".into()))
    );
    assert_eq!(
        result.rows[0].get("external_language"),
        Some(&Value::Str("PLPGSQL".into()))
    );
}

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
    // PG17: abcd - the inner handler recovers, outer code continues.
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
           -- c defaults to NULL (PG17).
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
    // PG17: 5, 10
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
    // Unnamed parameters are only addressable as $n (PG17: 304).
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
    // PG17: 1
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
    // Named argument in FROM position (PG17: 1, 2).
    let result = exec(&eng, "SELECT * FROM gen5(n => 2)");
    assert_eq!(result.rows.len(), 2);
    // Column alias list renames the output column (PG17: val = 2).
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
    // PG17: function amb(integer) is not unique.
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
fn percent_type_declarations_are_best_effort() {
    let eng = engine();
    exec(&eng, "CREATE TABLE typed (id INTEGER, name TEXT)");
    exec(&eng, "INSERT INTO typed VALUES (7, 'seven')");
    // %TYPE resolves to no cast (best effort); values pass through.
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
