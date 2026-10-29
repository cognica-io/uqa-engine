//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::MemoryBudget, ArrayValue, CancellationToken, LegacyVectorKind, LegacyVectorValue,
    TemporalValue,
};

fn cases() -> Vec<(Value, ColumnType)> {
    let mut cases = vec![
        (Value::Void, ColumnType::Void),
        (Value::Bool(false), ColumnType::Boolean),
        (Value::Int(1), ColumnType::Integer),
        (Value::Int(i64::MAX), ColumnType::BigInteger),
        (Value::Float(1.0), ColumnType::DoublePrecision),
        (
            Value::Decimal(uqa_core::DecimalValue::from_i64(3)),
            numeric_type(),
        ),
        (Value::Str("text".into()), ColumnType::Text),
        (Value::FixedChar("한국어".into()), ColumnType::Character(3)),
        (Value::FixedChar("longer".into()), ColumnType::Character(6)),
        (Value::Bytes(vec![0]), ColumnType::Bytea),
        (
            Value::BitString(uqa_core::BitStringValue::try_from_packed(vec![128], 1).unwrap()),
            ColumnType::Bit(None),
        ),
        (Value::Json("{}".into()), ColumnType::Json),
        (Value::JsonB("{}".into()), ColumnType::JsonB),
        (
            Value::Row(uqa_core::RowValue::new(vec![])),
            ColumnType::Record,
        ),
        (
            Value::Record(uqa_core::RecordValue::new(vec![])),
            ColumnType::Record,
        ),
    ];
    for (value, ty) in [
        (TemporalValue::Date { days: 0 }, ColumnType::Date),
        (TemporalValue::Time { micros: 0 }, ColumnType::Time),
        (
            TemporalValue::TimeTz {
                micros: 0,
                offset_seconds: 0,
            },
            ColumnType::TimeTz,
        ),
        (
            TemporalValue::Timestamp { postgres_micros: 0 },
            ColumnType::Timestamp,
        ),
        (
            TemporalValue::TimestampTz { postgres_micros: 0 },
            ColumnType::TimestampTz,
        ),
        (
            TemporalValue::Interval {
                months: 0,
                days: 0,
                micros: 0,
            },
            ColumnType::Interval,
        ),
    ] {
        cases.push((Value::Temporal(value), ty));
    }
    for (kind, ty) in [
        (LegacyVectorKind::SmallInteger, ColumnType::Int2Vector),
        (LegacyVectorKind::Oid, ColumnType::OidVector),
    ] {
        cases.push((
            Value::LegacyVector(LegacyVectorValue::try_new(kind, vec![Value::Int(1)]).unwrap()),
            ty,
        ));
    }
    cases
}

#[test]
fn runtime_observation_preserves_scalar_common_types_and_nested_array_depth() {
    let control = ProductionControl::uncontrolled();
    let cases = cases();
    for (left_value, left_type) in &cases {
        for (right_value, right_type) in &cases {
            for (left_depth, right_depth) in [(0, 0), (1, 1), (1, 2), (2, 2), (0, 1), (1, 0)] {
                let nested = |value: &Value, ty: &ColumnType, depth| {
                    let (mut value, mut ty) = (value.clone(), ty.clone());
                    for _ in 0..depth {
                        value = Value::List(vec![value]);
                        ty = ColumnType::Array(Box::new(ty));
                    }
                    (value, ty)
                };
                let (left, left_type) = nested(left_value, left_type, left_depth);
                let (right, right_type) = nested(right_value, right_type, right_depth);
                // The independently owned SQL-type operation must select exactly the runtime observer's shape, including conflicts.
                let expected = crate::common_type(&left_type, &right_type)
                    .ok()
                    .map(|ty| ColumnType::Array(Box::new(ty)));
                let values = Value::List(vec![left, Value::Null, right]);
                let allocation = allocation_counter::measure(|| {
                    let observed = RuntimeType::infer(&values, &control).unwrap();
                    assert_eq!(
                        observed.is_some(),
                        expected.is_some(),
                        "{left_type:?} / {right_type:?}"
                    );
                    if let Some(observed) = observed {
                        assert!(
                            observed.matches(expected.as_ref().unwrap()),
                            "{left_type:?} / {right_type:?}"
                        );
                    }
                });
                assert_eq!(
                    allocation.count_total, 0,
                    "{left_type:?} / {right_type:?}: {allocation:?}"
                );
            }
        }
    }
}

#[test]
fn runtime_array_carriers_flatten_lists_without_flattening_array_valued_elements() {
    let nested = vec![
        Value::List(vec![Value::Int(1), Value::Null]),
        Value::List(vec![Value::Int(2), Value::Int(3)]),
    ];
    let flat_type = ColumnType::Array(Box::new(ColumnType::Integer));
    let nested_type = ColumnType::Array(Box::new(flat_type.clone()));
    let explicit = Value::Array(ArrayValue::try_new(nested.clone()).unwrap());
    let list = Value::List(nested);
    let array_element =
        Value::Array(ArrayValue::with_lower_bounds(vec![explicit.clone()], vec![1]).unwrap());
    for (value, expected) in [
        (explicit, &flat_type),
        (list, &nested_type),
        (array_element, &nested_type),
    ] {
        let allocation = allocation_counter::measure(|| {
            assert!(
                RuntimeType::infer(&value, &ProductionControl::uncontrolled())
                    .unwrap()
                    .unwrap()
                    .matches(expected)
            );
        });
        assert_eq!(allocation.count_total, 0, "{allocation:?}");
    }
}

#[test]
fn runtime_type_construction_admits_only_final_boxes_and_releases_partial_failure() {
    let values = Value::List(
        (0..4096)
            .map(|_| Value::List(vec![Value::Int(1)]))
            .collect(),
    );
    let required = 2 * size_of::<ColumnType>();
    let budget = MemoryBudget::new(required + 32);
    let sibling = budget.reserve(32).unwrap();
    let child = budget.child(required);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&child, &token, &token);
    let mut retained = None;
    let allocation = allocation_counter::measure(|| {
        retained = Some(
            super::super::value_type_with_control(&values, &control)
                .unwrap()
                .unwrap(),
        );
    });
    let retained = retained.unwrap();
    assert_eq!(allocation.count_total, 2, "{allocation:?}");
    assert_eq!(allocation.bytes_total, required as u64, "{allocation:?}");
    assert_eq!(retained.reserved_bytes(), required);
    assert_eq!(child.used(), required);
    assert_eq!(budget.used(), required + sibling.bytes());
    drop(retained);
    assert_eq!(budget.used(), sibling.bytes());
    let insufficient = budget.child(required - 1);
    let error = super::super::value_type_with_control(
        &values,
        &ProductionControl::new(&insufficient, &token, &token),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), sibling.bytes());
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = super::super::value_type_with_control(
            &values,
            &ProductionControl::new(&child, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), sibling.bytes());
    }
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _retained = super::super::value_type_with_control(&values, &control)
            .unwrap()
            .unwrap();
        panic!("runtime type consumer unwind");
    }))
    .is_err());
    assert_eq!(budget.used(), sibling.bytes());
}
