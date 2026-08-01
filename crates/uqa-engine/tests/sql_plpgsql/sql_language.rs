use super::*;

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
