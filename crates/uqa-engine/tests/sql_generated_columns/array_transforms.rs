//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column coverage for `PostgreSQL` 18 array transformations.

use uqa_core::{ArrayValue, Value};
use uqa_engine::Engine;

#[test]
fn generated_columns_preserve_array_transform_binding_and_comparison_errors() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_array_transforms (
                 source INTEGER[],
                 sorted INTEGER[] GENERATED ALWAYS AS (
                     array_sort(\"array\" => source, descending => 't')
                 ) STORED,
                 reversed INTEGER[] GENERATED ALWAYS AS (array_reverse(source)) VIRTUAL
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_array_transforms (source) VALUES (ARRAY[2,1])",
            &[],
        )
        .unwrap();
    let row = engine
        .sql(
            "SELECT sorted, reversed FROM generated_array_transforms",
            &[],
        )
        .unwrap()
        .rows
        .pop()
        .unwrap();
    assert_eq!(
        row["sorted"],
        Value::Array(ArrayValue::try_new(vec![Value::Int(2), Value::Int(1)]).unwrap())
    );
    assert_eq!(
        row["reversed"],
        Value::Array(ArrayValue::try_new(vec![Value::Int(1), Value::Int(2)]).unwrap())
    );

    for (expression, sqlstate) in [
        ("array_sort(NULL)", "42804"),
        ("array_sort(source, 1)", "42883"),
    ] {
        let sql = format!(
            "CREATE TABLE generated_bad_array_transform (
                 source INTEGER[],
                 generated INTEGER[] GENERATED ALWAYS AS ({expression}) STORED
             )"
        );
        let error = engine.sql(&sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(sqlstate), "{expression}: {error}");
    }

    engine
        .sql(
            "CREATE TABLE generated_json_array_sort (
                 source JSON[],
                 sorted JSON[] GENERATED ALWAYS AS (array_sort(source)) STORED
             )",
            &[],
        )
        .unwrap();
    let error = engine
        .sql(
            "INSERT INTO generated_json_array_sort (source) VALUES (ARRAY['{}'::json,'{}'::json])",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert_eq!(
        error.to_string(),
        "could not identify a comparison function for type json"
    );
    assert!(engine
        .sql("SELECT * FROM generated_json_array_sort", &[])
        .unwrap()
        .rows
        .is_empty());
}
