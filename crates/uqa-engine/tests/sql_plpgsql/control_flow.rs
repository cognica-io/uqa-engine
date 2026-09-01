//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
    // PG18: CASE with no matching arm and no ELSE => "case not found".
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
    // 1 + 3 + 5 + 7 + 9 = 25 (verified against PG18)
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
           -- PG18: REVERSE 1..3 iterates zero times.
           FOR i IN REVERSE 1..3 LOOP acc := acc || i; END LOOP;
           RETURN acc;
         END;
         $$ LANGUAGE plpgsql",
    );
    // Verified against PG18: 123:321:147:
    assert_eq!(
        scalar(&eng, "SELECT for_text() AS v"),
        Value::Str("123:321:147:".into())
    );
    // PG18: BY 0 => "BY value of FOR loop must be greater than zero".
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
fn foreach_array_elements_use_storage_order_control_flow_and_found() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION foreach_elements(items integer[]) RETURNS text AS $$
         DECLARE item integer; out_text text := ''; prior boolean;
         BEGIN
           PERFORM 1;
           prior := FOUND;
           <<walk>>
           FOREACH item SLICE 0 IN ARRAY items LOOP
             CONTINUE walk WHEN item = 2;
             EXIT walk WHEN item = 4;
             out_text := out_text || coalesce(item::text, 'NULL') || ',';
           END LOOP walk;
           RETURN out_text || '|found=' || FOUND::text || '|prior=' || prior::text;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT foreach_elements('[0:1][5:6]={{1,2},{3,4}}'::integer[]) AS v",
        ),
        Value::Str("1,3,|found=true|prior=true".into())
    );
    assert_eq!(
        scalar(&eng, "SELECT foreach_elements(ARRAY[]::integer[]) AS v",),
        Value::Str("|found=false|prior=true".into())
    );
}

#[test]
fn foreach_array_slices_preserve_dimensions_and_lower_bounds() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION foreach_slice_one(items integer[]) RETURNS text AS $$
         DECLARE item integer[]; out_text text := '';
         BEGIN
           FOREACH item SLICE 1 IN ARRAY items LOOP
             out_text := out_text || array_dims(item) || '=' || item::text || ';';
           END LOOP;
           RETURN out_text || '|found=' || FOUND::text;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT foreach_slice_one('[0:1][5:6]={{1,2},{3,4}}'::integer[]) AS v",
        ),
        Value::Str("[5:6]=[5:6]={1,2};[5:6]=[5:6]={3,4};|found=true".into())
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT foreach_slice_one('[3:4]={7,8}'::integer[]) AS v",
        ),
        Value::Str("[3:4]=[3:4]={7,8};|found=true".into())
    );

    exec(
        &eng,
        "CREATE FUNCTION foreach_slice_two(items integer[]) RETURNS text AS $$
         DECLARE item integer[]; out_text text := '';
         BEGIN
           FOREACH item SLICE 2 IN ARRAY items LOOP
             out_text := out_text || array_dims(item) || '=' || item::text || ';';
           END LOOP;
           RETURN out_text || '|found=' || FOUND::text;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT foreach_slice_two('[0:1][5:6]={{1,2},{3,4}}'::integer[]) AS v",
        ),
        Value::Str("[0:1][5:6]=[0:1][5:6]={{1,2},{3,4}};|found=true".into())
    );
}

#[test]
fn foreach_array_scalar_subquery_is_typed_and_evaluated_once() {
    let eng = engine();
    exec(&eng, "CREATE TABLE foreach_source_log(value integer)");
    exec(
        &eng,
        "CREATE FUNCTION foreach_source() RETURNS integer[] AS $$
         BEGIN
           INSERT INTO foreach_source_log VALUES (1);
           RETURN ARRAY[1, 2, 3];
         END;
         $$ LANGUAGE plpgsql VOLATILE",
    );
    exec(
        &eng,
        "CREATE FUNCTION foreach_subquery_sum() RETURNS integer AS $$
         DECLARE item integer; total integer := 0;
         BEGIN
           FOREACH item IN ARRAY (SELECT foreach_source()) LOOP
             total := total + item;
           END LOOP;
           RETURN total;
         END;
         $$ LANGUAGE plpgsql",
    );
    assert_eq!(
        scalar(&eng, "SELECT foreach_subquery_sum() AS value"),
        Value::Int(6)
    );
    assert_eq!(
        scalar(&eng, "SELECT count(*) FROM foreach_source_log"),
        Value::Int(1)
    );
}

#[test]
fn foreach_array_validates_expression_dimensions_and_target_type() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION foreach_null(items integer[]) RETURNS integer AS $$
         DECLARE item integer;
         BEGIN FOREACH item IN ARRAY items LOOP NULL; END LOOP; RETURN 0; END;
         $$ LANGUAGE plpgsql",
    );
    let err = exec_err(&eng, "SELECT foreach_null(NULL::integer[]) AS v");
    assert_eq!(err.sqlstate(), Some("22004"));
    assert!(
        err.to_string()
            .contains("FOREACH expression must not be null"),
        "got: {err}"
    );

    exec(
        &eng,
        "CREATE FUNCTION foreach_nonarray() RETURNS integer AS $$
         DECLARE item integer;
         BEGIN FOREACH item IN ARRAY 42 LOOP NULL; END LOOP; RETURN 0; END;
         $$ LANGUAGE plpgsql",
    );
    let err = exec_err(&eng, "SELECT foreach_nonarray() AS v");
    assert_eq!(err.sqlstate(), Some("42804"));
    assert!(
        err.to_string()
            .contains("FOREACH expression must yield an array, not type integer"),
        "got: {err}"
    );

    exec(
        &eng,
        "CREATE FUNCTION foreach_too_deep(items integer[]) RETURNS integer AS $$
         DECLARE item integer[];
         BEGIN FOREACH item SLICE 2 IN ARRAY items LOOP NULL; END LOOP; RETURN 0; END;
         $$ LANGUAGE plpgsql",
    );
    let err = exec_err(&eng, "SELECT foreach_too_deep(ARRAY[1,2]) AS v");
    assert_eq!(err.sqlstate(), Some("2202E"));
    assert!(
        err.to_string()
            .contains("slice dimension (2) is out of the valid range 0..1"),
        "got: {err}"
    );

    exec(
        &eng,
        "CREATE FUNCTION foreach_slice_scalar(items integer[]) RETURNS integer AS $$
         DECLARE item integer;
         BEGIN FOREACH item SLICE 1 IN ARRAY items LOOP NULL; END LOOP; RETURN 0; END;
         $$ LANGUAGE plpgsql",
    );
    let err = exec_err(&eng, "SELECT foreach_slice_scalar(ARRAY[1,2]) AS v");
    assert_eq!(err.sqlstate(), Some("42804"));
    assert!(
        err.to_string()
            .contains("FOREACH ... SLICE loop variable must be of an array type"),
        "got: {err}"
    );

    exec(
        &eng,
        "CREATE FUNCTION foreach_array_element(items integer[]) RETURNS integer AS $$
         DECLARE item integer[];
         BEGIN FOREACH item IN ARRAY items LOOP NULL; END LOOP; RETURN 0; END;
         $$ LANGUAGE plpgsql",
    );
    let err = exec_err(
        &eng,
        "SELECT foreach_array_element(ARRAY[]::integer[]) AS v",
    );
    assert_eq!(err.sqlstate(), Some("42804"));
    assert!(
        err.to_string()
            .contains("FOREACH loop variable must not be of an array type"),
        "got: {err}"
    );

    let err = exec_err(&eng, "SELECT foreach_slice_scalar(ARRAY[]::integer[]) AS v");
    assert_eq!(err.sqlstate(), Some("2202E"));
    assert!(
        err.to_string()
            .contains("slice dimension (1) is out of the valid range 0..0"),
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
