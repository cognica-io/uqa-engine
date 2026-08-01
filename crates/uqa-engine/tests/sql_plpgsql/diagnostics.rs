//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
