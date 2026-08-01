//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
