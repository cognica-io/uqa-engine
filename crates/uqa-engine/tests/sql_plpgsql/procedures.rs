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
    exec(
        &eng,
        "CREATE FUNCTION tablef(value int) RETURNS TABLE(result int) AS $$
         BEGIN RETURN QUERY SELECT value; END;
         $$ LANGUAGE plpgsql",
    );
    exec(
        &eng,
        "CREATE FUNCTION outf(IN value int, OUT result int) AS $$
         BEGIN result := value; END;
         $$ LANGUAGE plpgsql",
    );
    exec(
        &eng,
        "CREATE FUNCTION table_default(value int, optional int DEFAULT 1)
         RETURNS TABLE(result int) AS $$
         BEGIN RETURN QUERY SELECT value + optional; END;
         $$ LANGUAGE plpgsql",
    );
    for function in ["tablef", "outf"] {
        let err = exec_err(&eng, &format!("CALL {function}(1)"));
        assert_eq!(err.sqlstate(), Some("42883"), "got: {err}");
        assert!(
            err.to_string()
                .contains(&format!("procedure {function}(integer) does not exist")),
            "got: {err}"
        );
        let err = exec_err(&eng, &format!("CALL {function}(1, NULL)"));
        assert_eq!(err.sqlstate(), Some("42809"), "got: {err}");
        assert!(err.to_string().contains("is not a procedure"), "got: {err}");
    }
    let err = exec_err(&eng, "CALL table_default(1)");
    assert_eq!(err.sqlstate(), Some("42883"), "got: {err}");
    assert!(
        err.to_string()
            .contains("procedure table_default(integer) does not exist"),
        "got: {err}"
    );
    let err = exec_err(&eng, "CALL table_default(1, NULL)");
    assert_eq!(err.sqlstate(), Some("42809"), "got: {err}");
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
fn call_overload_resolution_prefers_float8_for_integer_input() {
    let eng = engine();
    exec(&eng, "CREATE TABLE procedure_pick_log (selected text)");
    exec(
        &eng,
        "CREATE PROCEDURE procedure_pick(value double precision) AS $$
         BEGIN INSERT INTO procedure_pick_log VALUES ('float8'); END;
         $$ LANGUAGE plpgsql",
    );
    exec(
        &eng,
        "CREATE PROCEDURE procedure_pick(value numeric) AS $$
         BEGIN INSERT INTO procedure_pick_log VALUES ('numeric'); END;
         $$ LANGUAGE plpgsql",
    );

    exec(&eng, "CALL procedure_pick(1)");
    assert_eq!(
        scalar(&eng, "SELECT selected FROM procedure_pick_log"),
        Value::Str("float8".into())
    );
}

#[test]
fn call_overload_resolution_preserves_declared_argument_types() {
    let eng = engine();
    exec(&eng, "CREATE TABLE procedure_width_log (selected text)");
    exec(
        &eng,
        "CREATE PROCEDURE procedure_width(value integer, note text DEFAULT 'default') AS $$
         BEGIN INSERT INTO procedure_width_log VALUES ('int4:' || note); END;
         $$ LANGUAGE plpgsql",
    );
    exec(
        &eng,
        "CREATE PROCEDURE procedure_width(value bigint, note text DEFAULT 'default') AS $$
         BEGIN INSERT INTO procedure_width_log VALUES ('int8:' || note); END;
         $$ LANGUAGE plpgsql",
    );

    exec(&eng, "CALL procedure_width(1::bigint)");
    exec(
        &eng,
        "CALL procedure_width(value => 2::bigint, note => 'named')",
    );
    let error = exec_err(
        &eng,
        "CALL procedure_width(value => (SELECT 2::bigint), note => 'subquery')",
    );
    assert_eq!(error.sqlstate(), Some("0A000"), "got: {error}");
    assert!(
        error
            .to_string()
            .contains("cannot use subquery in CALL argument"),
        "got: {error}"
    );
    exec(
        &eng,
        "CREATE FUNCTION function_width(value integer) RETURNS text AS $$
         BEGIN RETURN 'fn-int4'; END;
         $$ LANGUAGE plpgsql IMMUTABLE",
    );
    exec(
        &eng,
        "CREATE FUNCTION function_width(value bigint) RETURNS text AS $$
         BEGIN RETURN 'fn-int8'; END;
         $$ LANGUAGE plpgsql IMMUTABLE",
    );
    exec(
        &eng,
        "CREATE PROCEDURE procedure_width_caller(value bigint) AS $$
         DECLARE null_value bigint;
         BEGIN
           INSERT INTO procedure_width_log VALUES (function_width(value));
           CALL procedure_width(value, 'datum');
           INSERT INTO procedure_width_log VALUES (function_width(null_value));
           CALL procedure_width(null_value, 'null');
         END;
         $$ LANGUAGE plpgsql",
    );
    exec(&eng, "CALL procedure_width_caller(3::bigint)");
    let result = exec(
        &eng,
        "SELECT selected FROM procedure_width_log ORDER BY selected",
    );
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row.get("selected").cloned().unwrap_or(Value::Null))
            .collect::<Vec<_>>(),
        vec![
            Value::Str("fn-int8".into()),
            Value::Str("fn-int8".into()),
            Value::Str("int8:datum".into()),
            Value::Str("int8:default".into()),
            Value::Str("int8:named".into()),
            Value::Str("int8:null".into()),
        ]
    );
}

#[test]
fn procedure_overload_preserves_percent_type_null_variable() {
    let eng = engine();
    for sql in [
        "CREATE TABLE domain_carrier AS SELECT ordinal_position FROM information_schema.columns LIMIT 1",
        "CREATE TABLE domain_procedure_log(chosen TEXT)",
        "CREATE PROCEDURE domain_procedure(value INTEGER) LANGUAGE PLPGSQL AS $$ BEGIN INSERT INTO domain_procedure_log VALUES ('base'); END; $$",
        "CREATE PROCEDURE domain_procedure(value domain_carrier.ordinal_position%TYPE) LANGUAGE PLPGSQL AS $$ BEGIN INSERT INTO domain_procedure_log VALUES ('domain'); END; $$",
        "CREATE PROCEDURE invoke_domain_procedure() LANGUAGE PLPGSQL AS $$ DECLARE value domain_carrier.ordinal_position%TYPE; BEGIN CALL domain_procedure(value); END; $$",
    ] {
        exec(&eng, sql);
    }

    exec(&eng, "CALL invoke_domain_procedure()");
    assert_eq!(
        scalar(&eng, "SELECT chosen FROM domain_procedure_log"),
        Value::Str("domain".into())
    );
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
