//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken};

#[test]
fn controlled_comparison_preserves_native_numeric_jsonb_and_container_order() {
    let array = |values| Value::Array(ArrayValue::with_lower_bounds(values, vec![-2]).unwrap());
    let values = vec![
        Value::Null,
        Value::Void,
        Value::Bool(false),
        Value::Bool(true),
        Value::Int(i64::MIN),
        Value::Int(0),
        Value::Int(i64::MAX),
        Value::Float(-0.0),
        Value::Float(0.1),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NAN),
        Value::Decimal(DecimalValue::parse("0.10").unwrap()),
        Value::Decimal(DecimalValue::parse("1.000").unwrap()),
        Value::Decimal(DecimalValue::parse("NaN").unwrap()),
        Value::Str("a".into()),
        Value::FixedChar("a  ".into()),
        Value::Bytes(vec![0, 255]),
        Value::Json("[1]".into()),
        Value::JsonB("[]".into()),
        Value::JsonB("null".into()),
        Value::JsonB("{\"aa\":2,\"b\":1,\"b\":3}".into()),
        Value::JsonB("{\"b\":3.0,\"aa\":2}".into()),
        Value::JsonB("invalid".into()),
        array(vec![Value::Int(1), Value::Null]),
        array(vec![Value::Int(2)]),
        Value::List(vec![Value::Null]),
        Value::Row(vec![Value::Int(1), Value::Null]),
        Value::Record(vec![("ignored".into(), Value::Int(1))]),
        Value::Map([("key".into(), Value::JsonB("[1,2]".into()))].into()),
    ];
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for left in &values {
        for right in &values {
            assert_eq!(
                left.cmp_with_control(right, &control).unwrap(),
                left.cmp(right),
                "{left:?} / {right:?}"
            );
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn comparison_workspace_rejects_quota_and_preserves_both_cancellation_scopes() {
    let left = Value::JsonB("{\"nested\":[1,2,3]}".into());
    let right = Value::JsonB("{\"nested\":[1,2,4]}".into());
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert!(matches!(
        left.cmp_with_control(&right, &control),
        Err(ValueRetentionError::Memory(_))
    ));
    assert_eq!(budget.used(), 0);
    assert_eq!(
        Value::Int(1)
            .cmp_with_control(&Value::Int(2), &control)
            .unwrap(),
        Ordering::Less
    );
    for original_cancelled in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert!(matches!(
            left.cmp_with_control(&right, &control),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(budget.used(), 0);
    }
}
