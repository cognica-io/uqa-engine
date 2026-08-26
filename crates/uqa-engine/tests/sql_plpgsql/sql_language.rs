//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn downgrade_serialized_array_subscripts(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(values) => values
            .iter_mut()
            .map(downgrade_serialized_array_subscripts)
            .sum(),
        serde_json::Value::Object(object) => {
            let mut changed = 0;
            if let Some(serde_json::Value::Object(function)) = object.get_mut("Func") {
                let is_array_subscript = function
                    .get("binding")
                    .and_then(|binding| binding.get("dispatch"))
                    .and_then(serde_json::Value::as_str)
                    == Some("ArraySubscripts");
                if is_array_subscript {
                    function.insert(
                        "name".into(),
                        serde_json::Value::String("__array_subscripts".into()),
                    );
                    function.remove("binding");
                    changed += 1;
                }
            }
            changed
                + object
                    .values_mut()
                    .map(downgrade_serialized_array_subscripts)
                    .sum::<usize>()
        }
        _ => 0,
    }
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
    // An empty SETOF result produces zero rows in FROM (PG18).
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
fn sql_language_preserves_quoted_parameter_case() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION sql_quoted_parameter(\"InputValue\" int) RETURNS int AS $$
           SELECT \"InputValue\"
         $$ LANGUAGE sql",
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT sql_quoted_parameter(\"InputValue\" => 9) AS value",
        ),
        Value::Int(9)
    );
    assert_eq!(
        exec_err(
            &eng,
            "SELECT sql_quoted_parameter(inputvalue => 9) AS value",
        )
        .sqlstate(),
        Some("42883")
    );
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
fn v016_sql_standard_body_dispatch_markers_migrate_before_catalog_binding() {
    use uqa_storage::{Catalog, ManagedConnection};

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("legacy-function-dispatch.db");
    {
        let engine = Engine::open(&database).unwrap();
        exec(
            &engine,
            "CREATE FUNCTION legacy_array_pick(items integer[]) RETURNS integer
             LANGUAGE SQL IMMUTABLE RETURN items[2]",
        );
        assert_eq!(
            scalar(&engine, "SELECT legacy_array_pick(ARRAY[4, 9]) AS v"),
            Value::Int(9)
        );
    }

    {
        let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
        let encoded = catalog.get_metadata("sql_functions_json").unwrap().unwrap();
        let mut definitions: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            downgrade_serialized_array_subscripts(&mut definitions),
            1,
            "unexpected stored function definitions: {definitions}"
        );
        catalog
            .set_metadata(
                "sql_functions_json",
                &serde_json::to_string(&definitions).unwrap(),
            )
            .unwrap();
    }

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT legacy_array_pick(ARRAY[11, 42]) AS v",),
        Value::Int(42)
    );
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

    let ordinal = exec(
        &eng,
        "SELECT * FROM pairs(10) WITH ORDINALITY AS p(a, b, n) ORDER BY n",
    );
    assert_eq!(ordinal.columns, ["a", "b", "n"]);
    assert_eq!(
        ordinal.column_types,
        [
            Some(ColumnType::Integer),
            Some(ColumnType::Integer),
            Some(ColumnType::BigInteger),
        ]
    );
    assert_eq!(ordinal.value_at(2, 0), Some(&Value::Int(3)));
    assert_eq!(ordinal.value_at(2, 2), Some(&Value::Int(3)));
}
