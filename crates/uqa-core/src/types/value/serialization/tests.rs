//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryBudget, CancellationToken, DecimalValue};

#[test]
fn controlled_value_serialization_preserves_wire_bytes_and_releases_scratch() {
    let values = [
        Value::Null,
        Value::Void,
        Value::Float(f64::NAN),
        Value::Float(f64::NEG_INFINITY),
        Value::Str("\"line\n한글".into()),
        Value::FixedChar("x  ".into()),
        Value::Bytes(vec![0, 1, 127, 255]),
        Value::Decimal(DecimalValue::parse("123456789012345678901234567890.00100").unwrap()),
        Value::Json(" {\"a\":1} ".into()),
        Value::JsonB("{\"a\": 1}".into()),
        Value::Row(vec![Value::Bytes(vec![1, 2]), Value::Int(3)].into()),
        Value::Record(crate::RecordValue::from_parts(
            vec![("a".into(), Value::Bytes(vec![9, 0]))],
            Some(91),
        )),
        Value::Map(std::collections::BTreeMap::from([(
            "nested".into(),
            Value::List(vec![Value::Decimal(DecimalValue::parse("0.0100").unwrap())]),
        )])),
    ];
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for value in values {
        let context = ValueSerialization::new(control);
        let expected = serde_json::to_vec(&value).unwrap();
        let observed = serde_json::to_vec(&context.value(&value)).unwrap();
        assert!(context.take_error().is_none());
        assert_eq!(observed, expected, "{value:?}");
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn serialization_denial_and_both_cancellations_preserve_the_typed_failure() {
    let value = Value::Bytes(vec![3; 4 << 20]);
    for mode in 0..3 {
        let budget = MemoryBudget::new(1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        match mode {
            1 => original.cancel(),
            2 => invoking.cancel(),
            _ => {}
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        let context = ValueSerialization::new(control);
        let mut failure = None;
        let allocations = allocation_counter::measure(|| {
            assert!(serde_json::to_writer(std::io::sink(), &context.value(&value)).is_err());
            failure = context.take_error();
        });
        assert!(failure.is_some());
        assert!(
            allocations.bytes_total < 16 * 1024,
            "large byte encoding escaped admission"
        );
        assert_eq!(budget.used(), 0);
    }
}
