//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{memory::MemoryError, DecimalValue, TemporalValue};

fn values() -> Vec<Value> {
    vec![
        Value::Null,
        Value::Void,
        Value::Bool(true),
        Value::Int(i64::MIN),
        Value::Float(-0.0),
        Value::Str("한글🙂\0".repeat(2000)),
        Value::FixedChar("padded  ".into()),
        Value::Json(" {\"b\":2, \"a\":1} ".into()),
        Value::JsonB("{\"a\":1,\"b\":2}".into()),
        Value::Bytes(vec![0, 1, 127, 128, 255]),
        Value::Temporal(TemporalValue::TimeTz {
            micros: 42,
            offset_minutes: -330,
        }),
        Value::Decimal(DecimalValue::parse("-12345678901234567890.00100").unwrap()),
        Value::Decimal(DecimalValue::parse("NaN").unwrap()),
        Value::Decimal(DecimalValue::parse("Infinity").unwrap()),
        Value::Array(
            ArrayValue::with_lower_bounds(
                vec![
                    Value::List(vec![Value::Int(1), Value::Null]),
                    Value::List(vec![Value::Int(2), Value::Int(3)]),
                ],
                vec![-3, 9],
            )
            .unwrap(),
        ),
        Value::Array(ArrayValue::try_new(Vec::new()).unwrap()),
        Value::List(vec![
            Value::Bytes(vec![3; 8193]),
            Value::Row(vec![Value::Void]),
        ]),
        Value::Row(vec![Value::Int(7), Value::Null]),
        Value::Record(vec![
            ("duplicate".into(), Value::Int(1)),
            ("duplicate".into(), Value::Str("second".into())),
        ]),
        Value::Map(
            [(
                "nested".into(),
                Value::Record(vec![("field".into(), Value::Row(vec![Value::Null]))]),
            )]
            .into(),
        ),
        Value::List(Vec::new()),
        Value::Row(Vec::new()),
        Value::Record(Vec::new()),
        Value::Map(BTreeMap::new()),
    ]
}

#[test]
fn owned_copies_preserve_all_value_variants_and_only_retain_destination_payloads() {
    let budget = MemoryBudget::new(1 << 22);
    let cancellation = CancellationToken::new();
    for value in values() {
        let copied = value.clone_budgeted(&budget, &cancellation).unwrap();
        assert_eq!(*copied, value);
        assert_eq!(
            std::mem::discriminant(&*copied),
            std::mem::discriminant(&value)
        );
        assert_eq!(
            copied.reserved_bytes(),
            copied
                .retained_payload_bytes(&budget, &cancellation)
                .unwrap()
        );
        assert_eq!(budget.used(), copied.reserved_bytes());
        if let (Value::Decimal(source), Value::Decimal(copied)) = (&value, &*copied) {
            assert_eq!(source.display_scale(), copied.display_scale());
        }
        if let (Value::Array(source), Value::Array(copied)) = (&value, &*copied) {
            assert_eq!(source.dimensions(), copied.dimensions());
            assert_eq!(source.lower_bounds(), copied.lower_bounds());
        }
        drop(copied);
        assert_eq!(budget.used(), 0);
    }
    for bits in [
        (-0.0_f64).to_bits(),
        f64::INFINITY.to_bits(),
        0x7ff8_1234_5678_9abc,
    ] {
        let copied = Value::Float(f64::from_bits(bits))
            .clone_budgeted(&budget, &cancellation)
            .unwrap();
        let Value::Float(value) = *copied else {
            panic!("float copy");
        };
        assert_eq!(value.to_bits(), bits);
    }
}

#[test]
fn copied_payloads_are_independent_and_do_not_adopt_source_spare_capacity() {
    let mut text = String::with_capacity(1 << 18);
    text.push_str("owned");
    let mut source = Value::Str(text);
    let budget = MemoryBudget::new(64);
    let copied = source
        .clone_budgeted(&budget, &CancellationToken::new())
        .unwrap();
    let (Value::Str(original), Value::Str(copy)) = (&mut source, &*copied) else {
        panic!("string values");
    };
    assert_ne!(original.as_ptr(), copy.as_ptr());
    original.replace_range(.., "changed");
    assert_eq!(copy, "owned");
    assert_eq!(budget.used(), copy.capacity());
    drop(source);
    assert_eq!(*copied, Value::Str("owned".into()));
    drop(copied);
    assert_eq!(budget.used(), 0);
}

#[test]
fn empty_value_carriers_and_inline_scalars_need_no_payload_or_traversal_allocation() {
    let budget = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    for source in [
        Value::Null,
        Value::Void,
        Value::Bool(false),
        Value::Int(3),
        Value::Float(0.25),
        Value::Str(String::new()),
        Value::Bytes(Vec::new()),
        Value::List(Vec::new()),
        Value::Row(Vec::new()),
        Value::Record(Vec::new()),
        Value::Map(BTreeMap::new()),
    ] {
        assert_eq!(
            *source.clone_budgeted(&budget, &cancellation).unwrap(),
            source
        );
    }
    assert_eq!(budget.peak(), 0);
}

#[test]
fn rejected_partial_copies_release_their_workspace_and_preserve_other_leases() {
    let source = Value::Map(
        [
            ("first".into(), Value::Str("small".repeat(100))),
            ("later".into(), Value::Bytes(vec![7; 1 << 17])),
        ]
        .into(),
    );
    for limit in [64, 512, 2048, 8192, 65536] {
        let budget = MemoryBudget::new(limit);
        let existing = budget.reserve(16).unwrap();
        assert!(matches!(
            source.clone_budgeted(&budget, &CancellationToken::new()),
            Err(ValueRetentionError::Memory(MemoryError::Limit { .. }))
        ));
        assert_eq!(budget.used(), 16);
        drop(existing);
        assert_eq!(budget.used(), 0);
    }
    let Value::Map(fields) = source else {
        panic!("source map");
    };
    assert_eq!(fields["later"], Value::Bytes(vec![7; 1 << 17]));
}

#[test]
fn cancellation_during_chunked_payload_copy_and_after_its_last_step_discards_the_result() {
    for source in [
        Value::Str("한🙂".repeat(10_000)),
        Value::Bytes(vec![8; 100_000]),
    ] {
        let budget = MemoryBudget::new(1 << 20);
        let mut checks = 0;
        let mut at_check = |fail_at| {
            let cancellation = CancellationToken::new();
            copy(&source, &budget, &mut || {
                checks += 1;
                if checks == fail_at {
                    cancellation.cancel();
                }
                cancellation.check().map_err(Into::into)
            })
        };
        let copied = at_check(usize::MAX).unwrap();
        drop(copied);
        let last = checks;
        for fail_at in [1, 7, last] {
            let cancellation = CancellationToken::new();
            let mut checks = 0;
            assert!(matches!(
                copy(&source, &budget, &mut || {
                    checks += 1;
                    if checks == fail_at {
                        cancellation.cancel();
                    }
                    cancellation.check().map_err(Into::into)
                }),
                Err(ValueRetentionError::Cancelled(_))
            ));
            assert_eq!(checks, fail_at);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn nested_value_copy_uses_a_charged_traversal_stack_and_preserves_record_order() {
    let budget = MemoryBudget::new(1 << 22);
    let cancellation = CancellationToken::new();
    let mut source = Value::Record(vec![("z".into(), Value::Int(1)), ("a".into(), Value::Void)]);
    for _ in 0..256 {
        source = Value::List(vec![source]);
    }
    let copied = source.clone_budgeted(&budget, &cancellation).unwrap();
    assert_eq!(*copied, source);
    assert!(budget.peak() > copied.reserved_bytes());
    let mut leaf = &*copied;
    for _ in 0..256 {
        let Value::List(values) = leaf else {
            panic!("nested list");
        };
        leaf = &values[0];
    }
    let Value::Record(fields) = leaf else {
        panic!("record leaf");
    };
    assert_eq!(
        fields
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["z", "a"]
    );
    drop(copied);
    assert_eq!(budget.used(), 0);
}
