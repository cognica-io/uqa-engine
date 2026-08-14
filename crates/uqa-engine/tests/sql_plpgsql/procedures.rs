//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn procedure_with_inout_via_call() {
    let eng = engine();
    exec(
        &eng,
        "CREATE PROCEDURE p_inout(INOUT x int, IN y int) AS $$
         BEGIN x := x + y; END;
         $$ LANGUAGE plpgsql",
    );
    // PG18: CALL returns a result row named after the INOUT param.
    let result = exec(&eng, "CALL p_inout(10, 5)");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get("x"), Some(&Value::Int(15)));
    assert_eq!(result.column_types, [Some(ColumnType::Integer)]);
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
    assert_eq!(result.column_types, [Some(ColumnType::Integer)]);
    // PG18 error shapes for kind confusion.
    let err = exec_err(&eng, "SELECT p_inout(1, 2) AS v");
    assert!(err.to_string().contains("is a procedure"), "got: {err}");
    exec(
        &eng,
        "CREATE FUNCTION plainf(x int) RETURNS int AS $$ BEGIN RETURN x; END; $$ LANGUAGE plpgsql",
    );
    let err = exec_err(&eng, "CALL plainf(1)");
    assert!(err.to_string().contains("is not a procedure"), "got: {err}");
    // PG18: CALL with the wrong arity names the procedure.
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
    // PG18: only procedural languages work in DO.
    let err = exec_err(&eng, "DO LANGUAGE sql $$ SELECT 1 $$");
    assert!(
        err.to_string().contains("language \"sql\" does not exist"),
        "got: {err}"
    );
}
