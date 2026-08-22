//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table functions in `FROM`, standalone `VALUES`, and scalar table-function
//! bodies, including series generation, unnesting, regular-expression splits,
//! and JSON object or array expansion.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::ast::ColumnType;

fn values(result: &uqa_engine::SQLResult, column: &str) -> Vec<Value> {
    result.rows.iter().map(|row| row[column].clone()).collect()
}

#[test]
fn generate_series_basic() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(1, 5) AS t(n)", &[])
        .unwrap();
    assert_eq!(
        values(&r, "n"),
        vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5)
        ]
    );
}

#[test]
fn pg_catalog_qualified_generate_series_uses_the_builtin() {
    let eng = Engine::new();
    let result = eng
        .sql(
            "SELECT n FROM pg_catalog.generate_series(1, 3) AS t(n)",
            &[],
        )
        .unwrap();
    assert_eq!(
        values(&result, "n"),
        vec![Value::Int(1), Value::Int(2), Value::Int(3)]
    );
}

#[test]
fn table_function_default_column_keeps_the_local_identifier_structured() {
    let eng = Engine::new();
    let builtin = eng
        .sql(
            "SELECT generate_series FROM pg_catalog.generate_series(1, 2)",
            &[],
        )
        .unwrap();
    assert_eq!(
        values(&builtin, "generate_series"),
        vec![Value::Int(1), Value::Int(2)]
    );

    eng.sql(
        "CREATE FUNCTION \"series.dot\"(n integer) RETURNS SETOF integer AS $$
           SELECT generate_series(1, n)
         $$ LANGUAGE sql",
        &[],
    )
    .unwrap();
    let quoted = eng
        .sql("SELECT \"series.dot\" FROM \"series.dot\"(2)", &[])
        .unwrap();
    assert_eq!(
        values(&quoted, "series.dot"),
        vec![Value::Int(1), Value::Int(2)]
    );
}

#[test]
fn generate_series_relation_alias_is_default_column_alias() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT x FROM generate_series(1, 3) AS x", &[])
        .unwrap();
    assert_eq!(
        values(&r, "x"),
        vec![Value::Int(1), Value::Int(2), Value::Int(3)]
    );
}

#[test]
fn table_functions_with_ordinality_append_a_typed_aliased_column() {
    let eng = Engine::new();
    let default_names = eng
        .sql("SELECT * FROM generate_series(4, 6) WITH ORDINALITY", &[])
        .unwrap();
    assert_eq!(default_names.columns, ["generate_series", "ordinality"]);
    assert_eq!(
        default_names.column_types,
        [Some(ColumnType::Integer), Some(ColumnType::BigInteger)]
    );
    assert_eq!(default_names.value_at(0, 0), Some(&Value::Int(4)));
    assert_eq!(default_names.value_at(0, 1), Some(&Value::Int(1)));
    assert_eq!(default_names.value_at(2, 1), Some(&Value::Int(3)));

    let explicit_aliases = eng
        .sql(
            "SELECT g.value, g.sequence \
             FROM generate_series(4, 5) WITH ORDINALITY AS g(value, sequence) \
             ORDER BY g.sequence",
            &[],
        )
        .unwrap();
    assert_eq!(explicit_aliases.columns, ["value", "sequence"]);
    assert_eq!(explicit_aliases.value_at(1, 0), Some(&Value::Int(5)));
    assert_eq!(explicit_aliases.value_at(1, 1), Some(&Value::Int(2)));

    let partial_alias = eng
        .sql(
            "SELECT * FROM generate_series(4, 4) WITH ORDINALITY AS g(value)",
            &[],
        )
        .unwrap();
    assert_eq!(partial_alias.columns, ["value", "ordinality"]);

    let empty = eng
        .sql(
            "SELECT * FROM generate_series(5, 1) WITH ORDINALITY AS g(value, sequence)",
            &[],
        )
        .unwrap();
    assert_eq!(empty.columns, ["value", "sequence"]);
    assert_eq!(
        empty.column_types,
        [Some(ColumnType::Integer), Some(ColumnType::BigInteger)]
    );
    assert!(empty.rows.is_empty());

    let error = eng
        .sql(
            "SELECT * FROM generate_series(1, 1) WITH ORDINALITY AS g(a, b, c)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P10"));
}

#[test]
fn table_function_ordinality_resets_for_each_lateral_invocation() {
    let eng = Engine::new();
    let result = eng
        .sql(
            "SELECT v.n, g.value, g.ordinality \
             FROM (VALUES (2), (0), (1)) AS v(n) \
             CROSS JOIN LATERAL generate_series(1, v.n) \
             WITH ORDINALITY AS g(value, ordinality) \
             ORDER BY v.n DESC, g.ordinality",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(2)));
    assert_eq!(result.value_at(0, 1), Some(&Value::Int(1)));
    assert_eq!(result.value_at(0, 2), Some(&Value::Int(1)));
    assert_eq!(result.value_at(1, 1), Some(&Value::Int(2)));
    assert_eq!(result.value_at(1, 2), Some(&Value::Int(2)));
    assert_eq!(result.value_at(2, 0), Some(&Value::Int(1)));
    assert_eq!(result.value_at(2, 2), Some(&Value::Int(1)));
}

#[test]
fn multi_column_table_functions_put_ordinality_last() {
    let eng = Engine::new();
    let unnested = eng
        .sql(
            "SELECT * \
             FROM unnest(ARRAY[1, 2], ARRAY['x']) WITH ORDINALITY AS u(a, b, n)",
            &[],
        )
        .unwrap();
    assert_eq!(unnested.columns, ["a", "b", "n"]);
    assert_eq!(
        unnested.column_types,
        [
            Some(ColumnType::Integer),
            Some(ColumnType::Text),
            Some(ColumnType::BigInteger),
        ]
    );
    assert_eq!(unnested.value_at(1, 0), Some(&Value::Int(2)));
    assert_eq!(unnested.value_at(1, 1), Some(&Value::Null));
    assert_eq!(unnested.value_at(1, 2), Some(&Value::Int(2)));

    let json = eng
        .sql(
            "SELECT * FROM json_each('{\"a\": 1}') WITH ORDINALITY AS j(k)",
            &[],
        )
        .unwrap();
    assert_eq!(json.columns, ["k", "value", "ordinality"]);
    assert_eq!(json.value_at(0, 2), Some(&Value::Int(1)));
}

#[test]
fn generate_series_with_step() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(0, 10, 3) AS t(n)", &[])
        .unwrap();
    assert_eq!(
        values(&r, "n"),
        vec![Value::Int(0), Value::Int(3), Value::Int(6), Value::Int(9)]
    );
}

#[test]
fn generate_series_descending() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(5, 1, -1) AS t(n)", &[])
        .unwrap();
    assert_eq!(
        values(&r, "n"),
        vec![
            Value::Int(5),
            Value::Int(4),
            Value::Int(3),
            Value::Int(2),
            Value::Int(1)
        ]
    );
}

#[test]
fn generate_series_single_value() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(1, 1) AS t(n)", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0]["n"], Value::Int(1));
}

#[test]
fn generate_series_empty_range() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT n FROM generate_series(5, 1) AS t(n)", &[])
        .unwrap();
    assert!(r.rows.is_empty());
}

#[test]
fn generate_series_rejects_fractional_and_non_finite_float_bounds() {
    let eng = Engine::new();

    for sql in [
        "SELECT * FROM generate_series(1.5, 3.0)",
        "SELECT * FROM generate_series('NaN'::REAL, 3.0)",
        "SELECT * FROM generate_series(1.0, 'Infinity'::REAL)",
    ] {
        eng.sql(sql, &[])
            .expect_err("float-to-integer series coercion must not truncate or saturate");
    }
}

#[test]
fn unnest_basic() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT val FROM unnest(ARRAY[10, 20, 30]) AS t(val)", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 3);
    assert_eq!(
        values(&r, "val"),
        vec![Value::Int(10), Value::Int(20), Value::Int(30)]
    );
}

#[test]
fn unnest_text_array() {
    let eng = Engine::new();
    let r = eng
        .sql(
            "SELECT val FROM unnest(ARRAY['a', 'b', 'c']) AS t(val)",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 3);
    assert_eq!(
        values(&r, "val"),
        vec![
            Value::Str("a".into()),
            Value::Str("b".into()),
            Value::Str("c".into())
        ]
    );
}

#[test]
fn multi_array_unnest_zips_to_the_longest_input_and_null_pads() {
    let eng = Engine::new();
    let aliased = eng
        .sql(
            "SELECT a, b
             FROM unnest(ARRAY[1, 2], ARRAY['foo', 'bar', 'baz']) AS u(a, b)",
            &[],
        )
        .unwrap();
    assert_eq!(aliased.columns, ["a", "b"]);
    assert_eq!(aliased.rows.len(), 3);
    assert_eq!(aliased.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(aliased.value_at(1, 0), Some(&Value::Int(2)));
    assert_eq!(aliased.value_at(2, 0), Some(&Value::Null));
    assert_eq!(aliased.value_at(2, 1), Some(&Value::Str("baz".into())));

    let default_names = eng
        .sql(
            "SELECT * FROM unnest(ARRAY[1, 2], ARRAY['foo', 'bar', 'baz'])",
            &[],
        )
        .unwrap();
    assert_eq!(default_names.columns, ["unnest", "unnest"]);
    assert_eq!(default_names.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(
        default_names.value_at(0, 1),
        Some(&Value::Str("foo".into()))
    );
    assert_eq!(default_names.value_at(2, 0), Some(&Value::Null));
    assert_eq!(
        default_names.value_at(2, 1),
        Some(&Value::Str("baz".into()))
    );

    let partially_aliased = eng
        .sql("SELECT * FROM unnest(ARRAY[1], ARRAY['foo']) AS u(a)", &[])
        .unwrap();
    assert_eq!(partially_aliased.columns, ["a", "unnest"]);
}

#[test]
fn multi_array_unnest_is_rejected_outside_from() {
    let eng = Engine::new();
    let error = eng
        .sql("SELECT unnest(ARRAY[1], ARRAY[2])", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
}

#[test]
fn values_in_from_with_aliased_columns() {
    let eng = Engine::new();
    let r = eng
        .sql("SELECT id FROM (VALUES (1), (2), (3)) AS t(id)", &[])
        .unwrap();
    assert_eq!(r.rows.len(), 3);
}

#[test]
fn standalone_values_returns_rows() {
    let eng = Engine::new();
    let r = eng.sql("VALUES (1, 'a'), (2, 'b')", &[]).unwrap();
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[0].get("column1"), Some(&Value::Int(1)));
    assert_eq!(r.rows[1].get("column2"), Some(&Value::Str("b".into())));
}

#[test]
fn values_coerce_unknown_literals_to_the_postgresql_common_type() {
    let eng = Engine::new();

    let dates = eng
        .sql("VALUES (DATE '2020-01-01'), ('2020-01-02')", &[])
        .unwrap();
    assert_eq!(dates.column_types, [Some(ColumnType::Date)]);
    assert!(dates
        .rows
        .iter()
        .all(|row| matches!(row["column1"], Value::Temporal(_))));

    let dates_from = eng
        .sql(
            "SELECT value FROM (VALUES (DATE '2020-01-01'), ('2020-01-02')) AS source(value)",
            &[],
        )
        .unwrap();
    assert_eq!(dates_from.column_types, [Some(ColumnType::Date)]);
    assert!(dates_from
        .rows
        .iter()
        .all(|row| matches!(row["value"], Value::Temporal(_))));

    let integers = eng.sql("VALUES (1), ('2')", &[]).unwrap();
    assert_eq!(integers.column_types, [Some(ColumnType::Integer)]);
    assert_eq!(integers.rows[1]["column1"], Value::Int(2));

    let booleans = eng.sql("VALUES (TRUE), ('false')", &[]).unwrap();
    assert_eq!(booleans.column_types, [Some(ColumnType::Boolean)]);
    assert_eq!(booleans.rows[1]["column1"], Value::Bool(false));
}
