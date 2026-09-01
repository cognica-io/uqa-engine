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

#[test]
fn procedural_commit_and_rollback_continue_in_fresh_transactions() {
    let eng = engine();
    exec(&eng, "CREATE TABLE transaction_log (value INTEGER)");
    exec(
        &eng,
        "CREATE PROCEDURE commit_then_continue() LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO transaction_log VALUES (1);
           COMMIT;
           INSERT INTO transaction_log VALUES (2);
         END $$",
    );
    exec(&eng, "CALL commit_then_continue()");
    let result = exec(&eng, "SELECT value FROM transaction_log ORDER BY value");
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row.get("value").cloned().unwrap_or(Value::Null))
            .collect::<Vec<_>>(),
        [Value::Int(1), Value::Int(2)]
    );

    exec(&eng, "TRUNCATE transaction_log");
    exec(
        &eng,
        "CREATE PROCEDURE rollback_then_continue() LANGUAGE plpgsql AS $$
         DECLARE local_value INTEGER := 10;
         BEGIN
           local_value := local_value + 1;
           INSERT INTO transaction_log VALUES (local_value);
           ROLLBACK;
           local_value := local_value + 1;
           INSERT INTO transaction_log VALUES (local_value);
         END $$",
    );
    exec(&eng, "CALL rollback_then_continue()");
    assert_eq!(
        scalar(&eng, "SELECT value FROM transaction_log"),
        Value::Int(12)
    );

    exec(
        &eng,
        "DO $$
         BEGIN
           INSERT INTO transaction_log VALUES (20);
           COMMIT AND CHAIN;
           INSERT INTO transaction_log VALUES (21);
           ROLLBACK AND CHAIN;
           INSERT INTO transaction_log VALUES (22);
         END $$",
    );
    let result = exec(&eng, "SELECT value FROM transaction_log ORDER BY value");
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row.get("value").cloned().unwrap_or(Value::Null))
            .collect::<Vec<_>>(),
        [Value::Int(12), Value::Int(20), Value::Int(22)]
    );
}

#[test]
fn procedural_transaction_control_rejects_atomic_execution_contexts() {
    let eng = engine();
    exec(&eng, "CREATE TABLE atomic_transaction_log (value TEXT)");
    exec(
        &eng,
        "CREATE FUNCTION function_commit() RETURNS INTEGER LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO atomic_transaction_log VALUES ('function');
           COMMIT;
           RETURN 1;
         END $$",
    );
    let error = exec_err(&eng, "SELECT function_commit()");
    assert_eq!(error.sqlstate(), Some("2D000"), "got: {error}");
    assert_eq!(
        scalar(&eng, "SELECT count(*) FROM atomic_transaction_log"),
        Value::Int(0)
    );

    exec(
        &eng,
        "CREATE PROCEDURE atomic_commit() LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO atomic_transaction_log VALUES ('procedure');
           COMMIT;
         END $$",
    );
    exec(&eng, "BEGIN");
    let error = exec_err(&eng, "CALL atomic_commit()");
    assert_eq!(error.sqlstate(), Some("2D000"), "got: {error}");
    let error = exec_err(&eng, "SELECT 1");
    assert_eq!(error.sqlstate(), Some("25P02"), "got: {error}");
    exec(&eng, "ROLLBACK");

    exec(
        &eng,
        "CREATE PROCEDURE catch_subtransaction_commit() LANGUAGE plpgsql AS $$
         BEGIN
           BEGIN
             INSERT INTO atomic_transaction_log VALUES ('discarded');
             COMMIT;
           EXCEPTION WHEN invalid_transaction_termination THEN
             INSERT INTO atomic_transaction_log VALUES ('caught:2D000');
           END;
         END $$",
    );
    exec(&eng, "CALL catch_subtransaction_commit()");
    assert_eq!(
        scalar(
            &eng,
            "SELECT string_agg(value, ',' ORDER BY value) FROM atomic_transaction_log"
        ),
        Value::Str("caught:2D000".into())
    );
    exec(&eng, "TRUNCATE atomic_transaction_log");

    exec(
        &eng,
        "CREATE PROCEDURE definer_commit() LANGUAGE plpgsql SECURITY DEFINER AS $$
         BEGIN
           INSERT INTO atomic_transaction_log VALUES ('definer');
           COMMIT;
         END $$",
    );
    let error = exec_err(&eng, "CALL definer_commit()");
    assert_eq!(error.sqlstate(), Some("2D000"), "got: {error}");
    exec(
        &eng,
        "CREATE PROCEDURE configured_commit() LANGUAGE plpgsql SET search_path = public AS $$
         BEGIN
           INSERT INTO atomic_transaction_log VALUES ('configured');
           COMMIT;
         END $$",
    );
    let error = exec_err(&eng, "CALL configured_commit()");
    assert_eq!(error.sqlstate(), Some("2D000"), "got: {error}");
    assert_eq!(
        scalar(&eng, "SELECT count(*) FROM atomic_transaction_log"),
        Value::Int(0)
    );

    exec(
        &eng,
        "CREATE PROCEDURE dynamic_commit() LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO atomic_transaction_log VALUES ('dynamic');
           EXECUTE 'COMMIT';
         END $$",
    );
    let error = exec_err(&eng, "CALL dynamic_commit()");
    assert_eq!(error.sqlstate(), Some("0A000"), "got: {error}");
    assert!(
        error
            .to_string()
            .contains("EXECUTE of transaction commands is not implemented"),
        "got: {error}"
    );
    assert_eq!(
        scalar(&eng, "SELECT count(*) FROM atomic_transaction_log"),
        Value::Int(0)
    );
}

#[test]
fn nested_procedure_transaction_control_requires_a_direct_call_chain() {
    let eng = engine();
    exec(&eng, "CREATE TABLE nested_transaction_log (value TEXT)");
    exec(
        &eng,
        "CREATE PROCEDURE inner_transaction() LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO nested_transaction_log VALUES ('inner-before');
           COMMIT;
           INSERT INTO nested_transaction_log VALUES ('inner-after');
         END $$",
    );
    exec(
        &eng,
        "CREATE PROCEDURE outer_direct_transaction() LANGUAGE plpgsql AS $$
         BEGIN
           CALL inner_transaction();
           INSERT INTO nested_transaction_log VALUES ('outer-after');
         END $$",
    );
    exec(&eng, "CALL outer_direct_transaction()");
    assert_eq!(
        scalar(
            &eng,
            "SELECT string_agg(value, ',' ORDER BY value) FROM nested_transaction_log"
        ),
        Value::Str("inner-after,inner-before,outer-after".into())
    );

    exec(&eng, "TRUNCATE nested_transaction_log");
    exec(
        &eng,
        "CREATE PROCEDURE outer_direct_do() LANGUAGE plpgsql AS $outer$
         BEGIN
           DO $inner$
           BEGIN
             INSERT INTO nested_transaction_log VALUES ('do-before');
             COMMIT;
             INSERT INTO nested_transaction_log VALUES ('do-after');
           END
           $inner$;
           INSERT INTO nested_transaction_log VALUES ('do-outer');
         END
         $outer$",
    );
    exec(&eng, "CALL outer_direct_do()");
    assert_eq!(
        scalar(
            &eng,
            "SELECT string_agg(value, ',' ORDER BY value) FROM nested_transaction_log"
        ),
        Value::Str("do-after,do-before,do-outer".into())
    );

    exec(&eng, "TRUNCATE nested_transaction_log");
    exec(
        &eng,
        "CREATE FUNCTION transaction_bridge() RETURNS INTEGER LANGUAGE plpgsql AS $$
         BEGIN
           CALL inner_transaction();
           RETURN 1;
         END $$",
    );
    exec(
        &eng,
        "CREATE PROCEDURE outer_bridged_transaction() LANGUAGE plpgsql AS $$
         BEGIN
           PERFORM transaction_bridge();
         END $$",
    );
    let error = exec_err(&eng, "CALL outer_bridged_transaction()");
    assert_eq!(error.sqlstate(), Some("2D000"), "got: {error}");
    assert_eq!(
        scalar(&eng, "SELECT count(*) FROM nested_transaction_log"),
        Value::Int(0)
    );

    let error = exec_err(&eng, "CALL inner_transaction(); SELECT 1");
    assert_eq!(error.sqlstate(), Some("2D000"), "got: {error}");
    assert_eq!(
        scalar(&eng, "SELECT count(*) FROM nested_transaction_log"),
        Value::Int(0)
    );
}

#[test]
fn later_procedure_errors_preserve_only_completed_transaction_segments() {
    let eng = engine();
    exec(
        &eng,
        "CREATE TABLE segmented_transaction_log (value INTEGER)",
    );
    exec(
        &eng,
        "CREATE PROCEDURE fail_after_commit() LANGUAGE plpgsql AS $$
         DECLARE quotient INTEGER;
         BEGIN
           INSERT INTO segmented_transaction_log VALUES (1);
           COMMIT;
           INSERT INTO segmented_transaction_log VALUES (2);
           quotient := 1 / 0;
         END $$",
    );
    let error = exec_err(&eng, "CALL fail_after_commit()");
    assert_eq!(error.sqlstate(), Some("22012"), "got: {error}");
    assert_eq!(eng.transaction_depth(), 0);
    assert_eq!(
        scalar(
            &eng,
            "SELECT string_agg(value::text, ',' ORDER BY value) FROM segmented_transaction_log"
        ),
        Value::Str("1".into())
    );
}

#[test]
fn procedural_transaction_control_preserves_select_loops_and_closes_explicit_cursors() {
    let eng = engine();
    exec(&eng, "CREATE TABLE transaction_loop_source (value INTEGER)");
    exec(
        &eng,
        "INSERT INTO transaction_loop_source VALUES (1), (2), (3), (4), (5), (6), (7), (8), (9), (10), (11), (12)",
    );
    exec(&eng, "CREATE TABLE transaction_loop_log (value TEXT)");
    exec(
        &eng,
        "CREATE PROCEDURE commit_in_select_loop() LANGUAGE plpgsql AS $$
         DECLARE row_value RECORD;
         BEGIN
           FOR row_value IN SELECT value FROM transaction_loop_source ORDER BY value LOOP
             INSERT INTO transaction_loop_log VALUES ('commit:' || row_value.value);
             COMMIT;
           END LOOP;
         END $$",
    );
    exec(&eng, "CALL commit_in_select_loop()");
    assert_eq!(
        scalar(&eng, "SELECT count(*) FROM transaction_loop_log"),
        Value::Int(12)
    );

    exec(&eng, "TRUNCATE transaction_loop_log");
    exec(
        &eng,
        "CREATE PROCEDURE rollback_in_select_loop() LANGUAGE plpgsql AS $$
         DECLARE row_value RECORD; visited INTEGER := 0;
         BEGIN
           FOR row_value IN SELECT value FROM transaction_loop_source ORDER BY value LOOP
             visited := visited + 1;
             INSERT INTO transaction_loop_log VALUES ('discard:' || row_value.value);
             ROLLBACK;
           END LOOP;
           INSERT INTO transaction_loop_log VALUES ('visited:' || visited);
         END $$",
    );
    exec(&eng, "CALL rollback_in_select_loop()");
    assert_eq!(
        scalar(&eng, "SELECT value FROM transaction_loop_log"),
        Value::Str("visited:12".into())
    );

    exec(&eng, "TRUNCATE transaction_loop_log");
    exec(
        &eng,
        "CREATE PROCEDURE explicit_cursor_commit() LANGUAGE plpgsql AS $$
         DECLARE cursor_value refcursor; fetched INTEGER;
         BEGIN
           OPEN cursor_value FOR SELECT value FROM transaction_loop_source ORDER BY value;
           FETCH cursor_value INTO fetched;
           INSERT INTO transaction_loop_log VALUES ('fetched:' || fetched);
           COMMIT;
           BEGIN
             FETCH cursor_value INTO fetched;
           EXCEPTION WHEN invalid_cursor_name THEN
             INSERT INTO transaction_loop_log VALUES ('closed:34000');
           END;
         END $$",
    );
    exec(&eng, "CALL explicit_cursor_commit()");
    assert_eq!(
        scalar(
            &eng,
            "SELECT string_agg(value, ',' ORDER BY value) FROM transaction_loop_log"
        ),
        Value::Str("closed:34000,fetched:1".into())
    );
}

#[test]
fn procedural_transaction_control_rejects_command_driven_cursor_loops() {
    let eng = engine();
    exec(&eng, "CREATE TABLE command_loop_source (value INTEGER)");
    exec(&eng, "INSERT INTO command_loop_source VALUES (1), (2), (3)");
    exec(&eng, "CREATE TABLE command_loop_target (value INTEGER)");
    exec(
        &eng,
        "CREATE PROCEDURE commit_in_command_loop() LANGUAGE plpgsql AS $$
         DECLARE row_value RECORD;
         BEGIN
           FOR row_value IN INSERT INTO command_loop_target SELECT value FROM command_loop_source RETURNING value LOOP
             COMMIT;
           END LOOP;
         END $$",
    );
    let error = exec_err(&eng, "CALL commit_in_command_loop()");
    assert_eq!(error.sqlstate(), Some("55000"), "got: {error}");
    assert!(
        error.to_string().contains(
            "cannot perform transaction commands inside a cursor loop that is not read-only"
        ),
        "got: {error}"
    );
    assert_eq!(
        scalar(&eng, "SELECT count(*) FROM command_loop_target"),
        Value::Int(0)
    );
}

#[test]
fn procedural_transaction_chain_controls_next_transaction_characteristics() {
    let eng = engine();
    exec(
        &eng,
        "CREATE TABLE procedural_characteristics (kind TEXT, isolation_level TEXT)",
    );
    exec(
        &eng,
        "CREATE PROCEDURE chained_characteristics() LANGUAGE plpgsql AS $$
         DECLARE isolation_level TEXT;
         BEGIN
           SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE;
           COMMIT AND CHAIN;
           SHOW transaction_isolation INTO isolation_level;
           INSERT INTO procedural_characteristics VALUES ('chain', isolation_level);
         END $$",
    );
    exec(&eng, "CALL chained_characteristics()");
    assert_eq!(
        scalar(
            &eng,
            "SELECT isolation_level FROM procedural_characteristics WHERE kind = 'chain'"
        ),
        Value::Str("read committed".into())
    );

    exec(
        &eng,
        "CREATE PROCEDURE unchained_characteristics() LANGUAGE plpgsql AS $$
         DECLARE isolation_level TEXT;
         BEGIN
           SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE;
           COMMIT;
           SHOW transaction_isolation INTO isolation_level;
           INSERT INTO procedural_characteristics VALUES ('no-chain', isolation_level);
         END $$",
    );
    exec(&eng, "CALL unchained_characteristics()");
    assert_eq!(
        scalar(
            &eng,
            "SELECT isolation_level FROM procedural_characteristics WHERE kind = 'no-chain'"
        ),
        Value::Str("serializable".into())
    );
}

#[test]
fn persistent_procedural_boundaries_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("procedural-transactions.db");
    {
        let eng = Engine::open(&database).unwrap();
        exec(
            &eng,
            "CREATE TABLE persistent_transaction_log (value INTEGER)",
        );
        exec(
            &eng,
            "CREATE PROCEDURE persistent_segments() LANGUAGE plpgsql AS $$
             DECLARE quotient INTEGER;
             BEGIN
               INSERT INTO persistent_transaction_log VALUES (1);
               COMMIT;
               INSERT INTO persistent_transaction_log VALUES (2);
               quotient := 1 / 0;
             END $$",
        );
        let error = exec_err(&eng, "CALL persistent_segments()");
        assert_eq!(error.sqlstate(), Some("22012"), "got: {error}");
        assert_eq!(eng.transaction_depth(), 0);
        assert_eq!(
            scalar(&eng, "SELECT value FROM persistent_transaction_log"),
            Value::Int(1)
        );
    }
    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT value FROM persistent_transaction_log"),
        Value::Int(1)
    );
}
