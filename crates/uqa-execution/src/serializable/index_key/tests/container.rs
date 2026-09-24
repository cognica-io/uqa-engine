//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite observations are checked against the selected native value index, including mixed numeric bounds and NULL elements.

use super::*;
use uqa_core::{ArrayValue, TemporalValue};

fn array(values: Vec<Value>) -> Value {
    Value::Array(ArrayValue::try_new(values).unwrap())
}

fn shifted(values: Vec<Value>, bounds: Vec<i32>) -> Value {
    Value::Array(ArrayValue::with_lower_bounds(values, bounds).unwrap())
}

#[test]
fn array_ranges_preserve_elements_dimensions_lower_bounds_and_fractional_cuts() {
    let values = vec![
        Value::Null,
        array(vec![]),
        array(vec![Value::Int(-1)]),
        array(vec![Value::Int(0)]),
        array(vec![Value::Int(1)]),
        array(vec![Value::Int(1), Value::Int(-9)]),
        array(vec![Value::Int(1), Value::Int(0)]),
        array(vec![Value::Int(1), Value::Int(1)]),
        array(vec![Value::Int(1), Value::Null]),
        array(vec![Value::Null]),
        array(vec![Value::List(vec![])]),
        array(vec![Value::List(vec![Value::Int(1), Value::Int(0)])]),
        shifted(vec![Value::Int(1), Value::Int(0)], vec![-2]),
        shifted(vec![Value::Int(1), Value::Int(0)], vec![2]),
        array(vec![Value::Int(i64::MIN)]),
        array(vec![Value::Int(i64::MAX)]),
    ];
    let mut targets = values.clone();
    targets.extend([
        array(vec![Value::Float(0.5), Value::Null]),
        array(vec![Value::Int(1), Value::Float(0.5)]),
        array(vec![decimal("1.00"), Value::Int(0)]),
        array(vec![Value::Float(f64::INFINITY)]),
        array(vec![Value::Float(f64::NEG_INFINITY)]),
        array(vec![decimal("9223372036854775808")]),
        array(vec![Value::Str("one".into())]),
        Value::Void,
        Value::Bool(true),
        Value::Str("{}".into()),
        Value::JsonB("[]".into()),
        Value::List(vec![]),
        Value::Row(vec![]),
        Value::Record(vec![]),
        Value::Map(std::collections::BTreeMap::new()),
    ]);
    verify(
        IndexDomain::Array(ScalarIndexDomain::Integer),
        &values,
        &targets,
    );
}

#[test]
fn float_array_cuts_canonicalize_signed_zero_and_preserve_nested_nan_order() {
    let mut values = vec![Value::Null, array(vec![]), array(vec![Value::Null])];
    for value in [
        f64::NEG_INFINITY,
        -f64::from_bits(1),
        -0.0,
        0.0,
        f64::from_bits(1),
        0.1,
        1.0,
        1.000_000_000_000_000_2,
        f64::INFINITY,
        f64::NAN,
    ] {
        values.push(array(vec![Value::Float(value)]));
        values.push(array(vec![Value::Float(value), Value::Float(1.0)]));
    }
    let mut targets = values.clone();
    targets.extend([
        array(vec![Value::Int(0)]),
        array(vec![decimal("0"), Value::Int(1)]),
        array(vec![decimal("0.100000000000000001"), Value::Null]),
        array(vec![decimal("1.0000000000000001"), Value::Int(-100)]),
        array(vec![Value::Int(i64::MAX)]),
        array(vec![decimal("NaN")]),
    ]);
    verify(
        IndexDomain::Array(ScalarIndexDomain::Float),
        &values,
        &targets,
    );
}

#[test]
fn legacy_vector_domains_observe_both_native_container_representations() {
    let values = vec![
        Value::Null,
        array(vec![]),
        Value::List(vec![]),
        array(vec![Value::Int(1)]),
        Value::List(vec![Value::Int(1)]),
        array(vec![Value::Int(1), Value::Int(2)]),
        Value::List(vec![Value::Int(1), Value::Int(2)]),
        array(vec![Value::Null]),
        Value::List(vec![Value::Null]),
    ];
    let mut targets = values.clone();
    targets.extend([
        array(vec![Value::Int(1), Value::Float(1.5)]),
        Value::List(vec![Value::Int(1), Value::Float(1.5)]),
        Value::Int(1),
        Value::Row(vec![]),
    ]);
    verify(IndexDomain::LegacyVector, &values, &targets);
}

#[test]
fn array_leaf_domains_keep_native_comparison_without_scalar_text_coercion() {
    for (domain, leaves, foreign) in [
        (
            ScalarIndexDomain::Text,
            vec![
                Value::Str(String::new()),
                Value::Str("a\0".into()),
                Value::Str("aa".into()),
            ],
            Value::FixedChar("a".into()),
        ),
        (
            ScalarIndexDomain::FixedChar,
            vec![
                Value::FixedChar("a".into()),
                Value::FixedChar("a  ".into()),
                Value::FixedChar("b".into()),
            ],
            Value::Str("a".into()),
        ),
        (
            ScalarIndexDomain::Bytes,
            vec![
                Value::Bytes(vec![]),
                Value::Bytes(vec![0]),
                Value::Bytes(vec![0, 255]),
            ],
            Value::Str(String::new()),
        ),
        (
            ScalarIndexDomain::JsonText,
            vec![Value::Json("[]".into()), Value::Json("{}".into())],
            Value::JsonB("[]".into()),
        ),
        (
            ScalarIndexDomain::JsonBinary,
            vec![
                Value::JsonB("[]".into()),
                Value::JsonB("{\"a\":1.0}".into()),
                Value::JsonB("{\"a\":1}".into()),
            ],
            Value::Json("[]".into()),
        ),
        (
            ScalarIndexDomain::Decimal,
            vec![
                decimal("-1"),
                decimal("0"),
                decimal("1.00"),
                decimal("1.01"),
            ],
            Value::Float(1.0),
        ),
        (
            ScalarIndexDomain::Temporal(TemporalIndexDomain::Date),
            vec![
                Value::Temporal(TemporalValue::Date { days: 0 }),
                Value::Temporal(TemporalValue::Date { days: 1 }),
            ],
            Value::Str("1970-01-01".into()),
        ),
    ] {
        let mut values = vec![Value::Null, array(vec![]), array(vec![Value::Null])];
        for leaf in leaves {
            values.extend([
                array(vec![leaf.clone()]),
                array(vec![leaf.clone(), Value::Null]),
                array(vec![leaf.clone(), leaf]),
            ]);
        }
        let mut targets = values.clone();
        targets.push(array(vec![foreign]));
        verify(IndexDomain::Array(domain), &values, &targets);
    }
}

#[test]
fn vector_and_tensor_keys_preserve_list_boundaries_and_native_leaf_comparison() {
    let vector =
        |values: &[f64]| Value::List(values.iter().map(|value| Value::Float(*value)).collect());
    let vectors = vec![
        Value::Null,
        vector(&[]),
        vector(&[-1.0]),
        vector(&[-0.0]),
        vector(&[0.0, 1.0]),
        vector(&[1.0]),
        Value::List(vec![Value::Null]),
    ];
    let mut targets = vectors.clone();
    targets.extend([
        Value::List(vec![Value::Int(0), Value::Int(1)]),
        Value::List(vec![decimal("0.000000000000000001")]),
        array(vec![]),
        Value::Row(vec![]),
    ]);
    verify(
        IndexDomain::List(ScalarIndexDomain::Float),
        &vectors,
        &targets,
    );

    let tensors = vec![
        Value::Null,
        Value::List(vec![]),
        Value::List(vec![vector(&[])]),
        Value::List(vec![vector(&[0.0, 1.0])]),
        Value::List(vec![vector(&[0.0]), vector(&[1.0])]),
        Value::List(vec![vector(&[1.0])]),
        Value::List(vec![Value::Null]),
    ];
    let mut targets = tensors.clone();
    targets.extend(vectors);
    targets.extend([
        Value::List(vec![Value::List(vec![Value::Int(0), Value::Int(1)])]),
        Value::List(vec![Value::List(vec![decimal("0.5"), Value::Null])]),
        Value::List(vec![Value::Row(vec![])]),
        array(vec![]),
        Value::Row(vec![]),
    ]);
    verify(IndexDomain::Tensor, &tensors, &targets);
}

#[test]
fn declared_container_domains_and_resource_errors_keep_original_control() {
    for (ty, domain) in [
        (ColumnType::Int2Vector, IndexDomain::LegacyVector),
        (ColumnType::OidVector, IndexDomain::LegacyVector),
        (
            ColumnType::Array(Box::new(ColumnType::Array(Box::new(ColumnType::Integer)))),
            IndexDomain::Array(ScalarIndexDomain::Integer),
        ),
        (
            ColumnType::Vector(2),
            IndexDomain::List(ScalarIndexDomain::Float),
        ),
        (ColumnType::Tensor(2), IndexDomain::Tensor),
    ] {
        assert_eq!(IndexDomain::from_column_type(&ty), Some(domain));
        let alias = ColumnType::Domain {
            schema: "public".into(),
            name: "container_value".into(),
            oid: 42_002,
            base: Box::new(ty),
        };
        assert_eq!(IndexDomain::from_column_type(&alias), Some(domain));
    }
    let value = array(vec![Value::Str("a".repeat(100))]);
    let domain = IndexDomain::Array(ScalarIndexDomain::Text);
    let control = StorageReadControl::with_limit(64);
    let error = domain.encode(&value, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(control.memory().used(), 0);
    let mut visited = false;
    let error = domain
        .visit_predicate(&Predicate::Equals(value.clone()), &control, &mut |_| {
            visited = true;
            Ok(())
        })
        .unwrap_err();
    assert!(!visited);
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    let error = domain.encode(&value, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(control.memory().used(), 0);
}

proptest! {
    #[test]
    fn mixed_array_predicates_match_actual_index_selection(
        values in prop::collection::vec(any::<i64>(), 0..5),
        targets in prop::collection::vec(-1.0e19_f64..1.0e19, 0..5),
    ) {
        let value = array(values.into_iter().map(Value::Int).collect());
        let target = array(targets.into_iter().map(Value::Float).collect());
        let index = ColumnValueIndex::build("key", std::iter::once((1, value.clone())));
        for predicate in [Predicate::Equals(target.clone()), Predicate::GreaterThan(target.clone()), Predicate::GreaterThanOrEqual(target.clone()), Predicate::LessThan(target.clone()), Predicate::LessThanOrEqual(target)] {
            prop_assert_eq!(observed(IndexDomain::Array(ScalarIndexDomain::Integer), &predicate, &value), !index.scan(&predicate).unwrap().is_empty());
        }
    }
}
