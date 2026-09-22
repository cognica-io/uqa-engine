//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ArrayValue, DecimalValue};

fn text(capacity: usize) -> String {
    let mut text = String::with_capacity(capacity);
    text.push('x');
    text
}

#[test]
fn scalar_and_spare_text_capacities_have_distinct_retention_costs() {
    let cancellation = CancellationToken::new();
    let empty = MemoryBudget::new(0);
    for value in [
        Value::Null,
        Value::Void,
        Value::Bool(true),
        Value::Int(7),
        Value::Float(1.5),
    ] {
        assert_eq!(
            value
                .reserve_retained_payload(&empty, &cancellation)
                .unwrap()
                .bytes(),
            0
        );
    }
    for value in [
        Value::Str(text(8192)),
        Value::FixedChar(text(8192)),
        Value::Json(text(8192)),
        Value::JsonB(text(8192)),
        Value::Bytes(Vec::with_capacity(8192)),
    ] {
        let small = MemoryBudget::new(4096);
        assert!(matches!(
            value.reserve_retained_payload(&small, &cancellation),
            Err(ValueRetentionError::Memory(MemoryError::Limit { .. }))
        ));
        assert_eq!(small.used(), 0);
        let large = MemoryBudget::new(1 << 20);
        let charge = value
            .reserve_retained_payload(&large, &cancellation)
            .unwrap();
        assert!(charge.bytes() >= 8192);
        assert_eq!(charge.bytes(), large.used());
        drop(charge);
        assert_eq!(large.used(), 0);
    }
}

#[test]
fn nested_collections_count_spare_slots_once_and_keep_field_names() {
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let mut values = Vec::with_capacity(32);
    values.push(Value::Str(text(2048)));
    let expected = values.capacity() * size_of::<Value>() + 2048;
    let charge = Value::List(values)
        .reserve_retained_payload(&budget, &cancellation)
        .unwrap();
    assert_eq!(charge.bytes(), expected);
    assert_eq!(budget.used(), expected);
    assert!(budget.peak() > budget.used());
    drop(charge);

    let mut fields = Vec::with_capacity(24);
    fields.push((
        text(512),
        Value::Row(vec![Value::Map(
            [(text(256), Value::Bytes(Vec::with_capacity(4096)))].into(),
        )]),
    ));
    let expected = fields.capacity() * size_of::<(String, Value)>()
        + 512
        + size_of::<Value>()
        + size_of::<(String, Value)>()
        + 256
        + 4096;
    let charge = Value::Record(fields)
        .reserve_retained_payload(&budget, &cancellation)
        .unwrap();
    assert_eq!(charge.bytes(), expected);
    drop(charge);
    assert_eq!(budget.used(), 0);
}

#[test]
fn arrays_keep_dimension_capacity_and_decimals_use_the_owning_representation() {
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let mut lower = Vec::with_capacity(128);
    lower.push(-7);
    let array = ArrayValue::with_lower_bounds(vec![Value::Str(text(1024))], lower).unwrap();
    let charge = Value::Array(array)
        .reserve_retained_payload(&budget, &cancellation)
        .unwrap();
    assert!(charge.bytes() >= 128 * size_of::<i32>() + size_of::<Value>() + 1024);
    drop(charge);
    let decimal = DecimalValue::parse("123456789012345678901234567890.12345").unwrap();
    let expected = decimal.retained_bytes();
    let charge = Value::Decimal(decimal)
        .reserve_retained_payload(&budget, &cancellation)
        .unwrap();
    assert_eq!(charge.bytes(), expected);
}

#[test]
fn rejected_deep_values_release_partial_charges_and_honor_cancellation() {
    let cancellation = CancellationToken::new();
    let budget = MemoryBudget::new(2048);
    let mut value = Value::Null;
    for _ in 0..4096 {
        value = Value::List(vec![value]);
    }
    assert!(matches!(
        value.reserve_retained_payload(&budget, &cancellation),
        Err(ValueRetentionError::Memory(MemoryError::Limit { .. }))
    ));
    assert_eq!(budget.used(), 0);
    cancellation.cancel();
    assert!(matches!(
        value.reserve_retained_payload(&budget, &cancellation),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(budget.used(), 0);
    while let Value::List(mut nested) = value {
        value = nested.pop().unwrap();
    }
}
