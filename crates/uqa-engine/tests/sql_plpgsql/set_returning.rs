//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
    // PG18: reaching the end of a SETOF function without RETURN is
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
fn set_valued_function_expands_inside_scalar_expression() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION setf() RETURNS SETOF int AS $$
         BEGIN RETURN NEXT 1; RETURN NEXT 2; END;
         $$ LANGUAGE plpgsql",
    );
    let result = exec(&eng, "SELECT abs(setf()) AS v");
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row["v"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2)]
    );
}

#[test]
fn select_list_set_functions_allow_postgresql_expression_contexts_and_preserve_types() {
    let eng = engine();

    let unary = exec(&eng, "SELECT -generate_series(1, 3) AS value");
    assert_eq!(
        unary
            .rows
            .iter()
            .map(|row| row["value"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(-1), Value::Int(-2), Value::Int(-3)]
    );

    let in_list = exec(&eng, "SELECT generate_series(1, 3) IN (1, 2) AS contained");
    assert_eq!(
        in_list
            .rows
            .iter()
            .map(|row| row["contained"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Bool(true), Value::Bool(true), Value::Bool(false)]
    );

    let arrays = exec(&eng, "SELECT ARRAY[generate_series(1, 2)] AS value");
    assert_eq!(
        arrays
            .rows
            .iter()
            .map(|row| row["value"].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::Array(uqa_core::ArrayValue::try_new(vec![Value::Int(1)]).unwrap()),
            Value::Array(uqa_core::ArrayValue::try_new(vec![Value::Int(2)]).unwrap()),
        ]
    );

    let typed = exec(
        &eng,
        "SELECT generate_series(1::bigint, 2::bigint) AS value",
    );
    assert_eq!(typed.column_types, [Some(ColumnType::BigInteger)]);
    let typed_subqueries = exec(
        &eng,
        "SELECT generate_series((SELECT 1::bigint), (SELECT 2::bigint)) AS value",
    );
    assert_eq!(
        typed_subqueries.column_types,
        [Some(ColumnType::BigInteger)]
    );
    assert_eq!(typed_subqueries.rows.len(), 2);
    let unnested = exec(&eng, "SELECT unnest(ARRAY[1::bigint, 2::bigint]) AS value");
    assert_eq!(unnested.column_types, [Some(ColumnType::BigInteger)]);

    let null_step = exec(&eng, "SELECT generate_series(1, 3, NULL) AS value");
    assert!(null_step.rows.is_empty());
}

#[test]
fn polymorphic_builtin_syntax_is_not_shadowed_by_a_user_set_function() {
    let eng = engine();
    exec(&eng, "CREATE SCHEMA syntax_shadow");
    exec(
        &eng,
        "CREATE FUNCTION syntax_shadow.coalesce(first_value TEXT, second_value TEXT) RETURNS SETOF TEXT AS $$
         BEGIN RETURN NEXT 'shadow'; RETURN NEXT 'shadow-2'; END;
         $$ LANGUAGE plpgsql",
    );
    exec(&eng, "SET search_path = syntax_shadow, pg_catalog, public");

    let result = exec(&eng, "SELECT coalesce(NULL::TEXT, 'builtin') AS value");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["value"], Value::Str("builtin".into()));
}

#[test]
fn set_projection_resolves_scalar_and_set_overloads_independently() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION overloaded_projection(value int) RETURNS int AS $$
         BEGIN RETURN value + 1; END;
         $$ LANGUAGE plpgsql",
    );
    exec(
        &eng,
        "CREATE FUNCTION overloaded_projection(value text) RETURNS SETOF text AS $$
         BEGIN RETURN NEXT value; END;
         $$ LANGUAGE plpgsql",
    );

    assert_eq!(
        scalar(
            &eng,
            "SELECT coalesce(overloaded_projection(1), 0) AS value",
        ),
        Value::Int(2)
    );
    let set = exec(
        &eng,
        "SELECT overloaded_projection('set-value'::text) AS value",
    );
    assert_eq!(set.rows[0]["value"], Value::Str("set-value".into()));

    exec(
        &eng,
        "CREATE TABLE overloaded_projection_input (int_value int, text_value text)",
    );
    exec(
        &eng,
        "INSERT INTO overloaded_projection_input VALUES (1, 'one'), (2, 'two')",
    );
    let scalar_columns = exec(
        &eng,
        "SELECT coalesce(overloaded_projection(int_value), 0) AS value FROM overloaded_projection_input ORDER BY int_value",
    );
    assert_eq!(
        scalar_columns
            .rows
            .iter()
            .map(|row| row["value"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(2), Value::Int(3)]
    );
    let set_columns = exec(
        &eng,
        "SELECT overloaded_projection(text_value) AS value FROM overloaded_projection_input ORDER BY text_value",
    );
    assert_eq!(
        set_columns
            .rows
            .iter()
            .map(|row| row["value"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Str("one".into()), Value::Str("two".into())]
    );
}

#[test]
fn set_projection_preserves_scalar_subquery_overload_binding() {
    let eng = engine();
    exec(
        &eng,
        "CREATE FUNCTION overloaded_set_projection(value int) RETURNS SETOF text AS $$
         BEGIN RETURN NEXT 'int4'; END;
         $$ LANGUAGE plpgsql",
    );
    exec(
        &eng,
        "CREATE FUNCTION overloaded_set_projection(value bigint) RETURNS SETOF text AS $$
         BEGIN RETURN NEXT 'int8'; END;
         $$ LANGUAGE plpgsql",
    );

    let selected = exec(
        &eng,
        "SELECT overloaded_set_projection((SELECT 7::BIGINT)) AS value",
    );
    assert_eq!(selected.rows[0]["value"], Value::Str("int8".into()));

    let ambiguous = exec_err(
        &eng,
        "SELECT overloaded_set_projection(1::SMALLINT) AS value",
    );
    assert_eq!(ambiguous.sqlstate(), Some("42725"), "{ambiguous}");
}

#[test]
fn select_list_set_functions_zip_expand_order_and_limit() {
    let eng = engine();
    let result = exec(
        &eng,
        "SELECT generate_series(1, 2) AS a, generate_series(10, 12) AS b",
    );
    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.rows[0]["a"], Value::Int(1));
    assert_eq!(result.rows[0]["b"], Value::Int(10));
    assert_eq!(result.rows[1]["a"], Value::Int(2));
    assert_eq!(result.rows[1]["b"], Value::Int(11));
    assert_eq!(result.rows[2]["a"], Value::Null);
    assert_eq!(result.rows[2]["b"], Value::Int(12));

    let result = exec(
        &eng,
        "SELECT generate_series(1, generate_series(1, 2)) AS n",
    );
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row["n"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(1), Value::Int(2)]
    );

    exec(&eng, "CREATE TABLE srf_input (id int)");
    exec(&eng, "INSERT INTO srf_input VALUES (2), (1)");
    let result = exec(
        &eng,
        "SELECT id, generate_series(1, id) AS n FROM srf_input ORDER BY id, n",
    );
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| (row["id"].clone(), row["n"].clone()))
            .collect::<Vec<_>>(),
        vec![
            (Value::Int(1), Value::Int(1)),
            (Value::Int(2), Value::Int(1)),
            (Value::Int(2), Value::Int(2)),
        ]
    );

    let result = exec(&eng, "SELECT generate_series(1, 5) AS n LIMIT 2");
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row["n"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2)]
    );

    let result = exec(
        &eng,
        "SELECT generate_series(1, CASE WHEN id = 1 THEN 2 ELSE 10 / (id - id) END) AS n FROM (VALUES (1), (2)) AS t(id) LIMIT 2",
    );
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row["n"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2)]
    );
}

#[test]
fn select_list_set_functions_expand_after_aggregate_and_window_phases() {
    let eng = engine();
    let result = exec(
        &eng,
        "SELECT count(*) AS c, generate_series(1, 2) AS n FROM (VALUES (1), (2)) AS t(x)",
    );
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0]["c"], Value::Int(2));
    assert_eq!(result.rows[0]["n"], Value::Int(1));
    assert_eq!(result.rows[1]["c"], Value::Int(2));
    assert_eq!(result.rows[1]["n"], Value::Int(2));

    let result = exec(
        &eng,
        "SELECT count(*) + generate_series(1, 2) AS n FROM (VALUES (1), (2)) AS t(x)",
    );
    assert_eq!(result.rows[0]["n"], Value::Int(3));
    assert_eq!(result.rows[1]["n"], Value::Int(4));

    let result = exec(
        &eng,
        "SELECT row_number() OVER (ORDER BY x) AS r, generate_series(1, 2) AS n FROM (VALUES (1), (2)) AS t(x)",
    );
    assert_eq!(result.rows.len(), 4);
    assert_eq!(result.rows[0]["r"], Value::Int(1));
    assert_eq!(result.rows[0]["n"], Value::Int(1));
    assert_eq!(result.rows[1]["r"], Value::Int(1));
    assert_eq!(result.rows[1]["n"], Value::Int(2));
    assert_eq!(result.rows[2]["r"], Value::Int(2));
    assert_eq!(result.rows[2]["n"], Value::Int(1));
    assert_eq!(result.rows[3]["r"], Value::Int(2));
    assert_eq!(result.rows[3]["n"], Value::Int(2));
}

#[test]
fn group_and_order_set_functions_use_postgresql_18_project_set_phases() {
    let eng = engine();
    let result = exec(&eng, "SELECT 1 AS v ORDER BY generate_series(1, 2)");
    assert_eq!(result.rows.len(), 2);

    let result = exec(
        &eng,
        "SELECT DISTINCT ON (generate_series(1, 2)) 1 AS v ORDER BY generate_series(1, 2)",
    );
    assert_eq!(result.rows.len(), 2);
    assert!(result.rows.iter().all(|row| row["v"] == Value::Int(1)));

    let result = exec(
        &eng,
        "SELECT generate_series(1, 2) AS x ORDER BY generate_series(10, 12)",
    );
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row["x"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2), Value::Null]
    );

    let result = exec(
        &eng,
        "SELECT count(*) AS c FROM (VALUES (1), (2)) AS t(x) ORDER BY generate_series(1, 2)",
    );
    assert_eq!(result.rows.len(), 2);
    assert!(result.rows.iter().all(|row| row["c"] == Value::Int(2)));

    let result = exec(
        &eng,
        "SELECT row_number() OVER () AS r FROM (VALUES (1), (2)) AS t(x) ORDER BY generate_series(1, 2)",
    );
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row["r"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(2), Value::Int(1), Value::Int(2)]
    );

    let result = exec(
        &eng,
        "SELECT generate_series(1, 2) AS n, count(*) AS c FROM (VALUES (1), (2)) AS t(x) GROUP BY generate_series(1, 2) ORDER BY 1",
    );
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0]["n"], Value::Int(1));
    assert_eq!(result.rows[0]["c"], Value::Int(2));
    assert_eq!(result.rows[1]["n"], Value::Int(2));
    assert_eq!(result.rows[1]["c"], Value::Int(2));

    let result = exec(
        &eng,
        "SELECT generate_series(1, 2), count(*) FROM (VALUES (1), (2)) AS t(x) GROUP BY generate_series(1, 2) ORDER BY 1",
    );
    assert_eq!(result.rows[0]["generate_series"], Value::Int(1));
    assert_eq!(result.rows[0]["count"], Value::Int(2));
    assert_eq!(result.rows[1]["generate_series"], Value::Int(2));
    assert_eq!(result.rows[1]["count"], Value::Int(2));

    let result = exec(
        &eng,
        "SELECT generate_series(1, 2) AS n, count(*) AS c GROUP BY generate_series(1, 3)",
    );
    assert_eq!(result.rows.len(), 6);
    assert_eq!(
        result
            .rows
            .iter()
            .filter(|row| row["n"] == Value::Int(1))
            .count(),
        3
    );
    assert_eq!(
        result
            .rows
            .iter()
            .filter(|row| row["n"] == Value::Int(2))
            .count(),
        3
    );
    assert!(result.rows.iter().all(|row| row["c"] == Value::Int(1)));
}

#[test]
fn select_list_set_functions_reject_pg18_forbidden_contexts() {
    let eng = engine();
    for (sql, expected) in [
        (
            "SELECT CASE WHEN true THEN generate_series(1, 2) END",
            "set-returning functions are not allowed in CASE",
        ),
        (
            "SELECT coalesce(generate_series(1, 2), 0)",
            "set-returning functions are not allowed in COALESCE",
        ),
        (
            "SELECT sum(generate_series(1, 2))",
            "aggregate function calls cannot contain set-returning function calls",
        ),
        (
            "SELECT lag(generate_series(1, 2)) OVER ()",
            "window function calls cannot contain set-returning function calls",
        ),
        (
            "SELECT NOT (generate_series(0, 1)::boolean)",
            "argument of NOT must not return a set",
        ),
        (
            "SELECT 1 IN (generate_series(1, 2))",
            "argument of IN must not return a set",
        ),
        (
            "SELECT 1 WHERE generate_series(1, 2) > 0",
            "set-returning functions are not allowed in WHERE",
        ),
        (
            "SELECT 1 HAVING generate_series(1, 2) > 0",
            "set-returning functions are not allowed in HAVING",
        ),
        (
            "SELECT 1 LIMIT generate_series(1, 2)",
            "set-returning functions are not allowed in LIMIT",
        ),
        (
            "SELECT 1 OFFSET generate_series(1, 2)",
            "set-returning functions are not allowed in OFFSET",
        ),
        (
            "SELECT 1 FROM (VALUES (1)) AS t(x) JOIN (VALUES (1)) AS u(y) ON generate_series(1, 2) > 0",
            "set-returning functions are not allowed in JOIN conditions",
        ),
        (
            "VALUES (generate_series(1, 2))",
            "set-returning functions are not allowed in VALUES",
        ),
        (
            "SELECT * FROM generate_series(1, generate_series(1, 2))",
            "set-returning functions must appear at top level of FROM",
        ),
    ] {
        let error = exec_err(&eng, sql);
        assert!(error.to_string().contains(expected), "got: {error}");
    }
}

// ---------------------------------------------------------------------
// Control flow
// ---------------------------------------------------------------------
