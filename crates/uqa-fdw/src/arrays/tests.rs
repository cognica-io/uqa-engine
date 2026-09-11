//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn domain(base: ColumnType) -> ColumnType {
    ColumnType::Domain {
        schema: "public".into(),
        name: "wrapped".into(),
        oid: 1,
        base: Box::new(base),
    }
}

#[test]
fn nested_domains_retain_their_array_or_scalar_base_classification() {
    assert!(column_type_is_array(&domain(domain(ColumnType::Array(
        Box::new(ColumnType::Integer)
    )))));
    assert!(!column_type_is_array(&domain(domain(ColumnType::Integer))));
    assert!(!column_type_is_array(&ColumnType::JsonB));
}

#[test]
fn rectangular_lists_become_sql_arrays_without_changing_unselected_columns() {
    let elements = vec![
        Value::List(vec![Value::Int(1), Value::Int(2)]),
        Value::List(vec![Value::Int(3), Value::Int(4)]),
    ];
    let untouched = Value::List(vec![Value::Str("payload".into())]);
    let result = normalize_array_columns(
        Row::from([
            ("matrix".into(), Value::List(elements.clone())),
            ("payload".into(), untouched.clone()),
        ]),
        &["matrix".into()],
    )
    .unwrap();
    let Value::Array(array) = &result["matrix"] else {
        panic!("expected array")
    };
    assert_eq!(array.dimensions(), &[2, 2]);
    assert_eq!(array.lower_bounds(), &[1, 1]);
    assert_eq!(array.elements(), &elements);
    assert_eq!(result["payload"], untouched);
}

#[test]
fn null_missing_values_and_existing_array_bounds_are_preserved() {
    let array = Value::Array(
        ArrayValue::with_lower_bounds(vec![Value::Int(7), Value::Int(8)], vec![-3]).unwrap(),
    );
    let row = Row::from([("bounded".into(), array), ("nullable".into(), Value::Null)]);
    assert_eq!(
        normalize_array_columns(
            row.clone(),
            &["bounded".into(), "nullable".into(), "absent".into()]
        )
        .unwrap(),
        row
    );
}

#[test]
fn ragged_dimensions_are_rejected_with_the_foreign_column_name() {
    let row = Row::from([(
        "matrix".into(),
        Value::List(vec![
            Value::List(vec![Value::Int(1)]),
            Value::List(vec![Value::Int(2), Value::Int(3)]),
        ]),
    )]);
    assert_eq!(
        normalize_array_columns(row, &["matrix".into()]).unwrap_err(),
        "foreign array column `matrix` contains non-rectangular dimensions"
    );
}

#[test]
fn scalar_values_are_rejected_for_declared_array_columns() {
    let row = Row::from([("items".into(), Value::Int(5))]);
    assert_eq!(
        normalize_array_columns(row, &["items".into()]).unwrap_err(),
        "foreign array column `items` requires an array value, got Int(5)"
    );
}
