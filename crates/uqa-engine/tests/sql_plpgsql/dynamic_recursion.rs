use super::*;

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
