//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ColumnType;

#[test]
fn admitted_constant_field_copy_preserves_physical_bytes_without_scalar_interpretation() {
    let cases = [
        (
            Value::Datum(DatumValue::new(3802, 0, vec![11, 42, 0, 0, 0])),
            ColumnType::JsonB,
            0,
            vec![32, 0, 0, 0, 42, 0, 0, 0],
        ),
        (
            Value::Datum(compressed(0, 9, &[8, b'a', b'b', b'c', 3, 3])),
            ColumnType::Text,
            0,
            [52_u32.to_le_bytes().as_slice(), b"abcabcabc"].concat(),
        ),
        (
            Value::Datum(DatumValue::new(23, u32::MAX, vec![1])),
            ColumnType::Integer,
            u32::MAX,
            vec![1],
        ),
    ];
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (source, ty, offset, expected) in cases {
        let output = copy_constant_field_with_control(&source, &ty, &control).unwrap();
        let Value::Datum(datum) = &*output else {
            panic!("physical output")
        };
        assert_eq!(datum.bytes(), expected);
        assert_eq!(datum.offset(), offset);
        assert_eq!(output.reserved_bytes(), budget.used());
        if ty == ColumnType::JsonB {
            assert_eq!(
                read(datum).unwrap_err().to_string(),
                "unknown type of jsonb container"
            );
        }
        assert_eq!(*output, copy_constant_field(&source, &ty).unwrap());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn constant_field_copy_admits_large_destinations_before_growth() {
    let mut bytes = (((4_u32 << 20) + 4) << 2).to_le_bytes().to_vec();
    bytes.resize((4 << 20) + 4, b'x');
    for source in [
        Value::Str("x".repeat(4 << 20)),
        Value::Datum(DatumValue::new(25, 0, bytes)),
    ] {
        for limit in [0, 1024] {
            let budget = MemoryBudget::new(limit);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let measured = allocation_counter::measure(|| {
                let error = copy_constant_field_with_control(&source, &ColumnType::Text, &control)
                    .unwrap_err();
                assert_eq!(error.sqlstate(), Some("53200"));
            });
            assert!(measured.bytes_total < 4096, "{measured:?}");
            assert_eq!(measured.bytes_current, 0);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn constant_field_copy_releases_decompression_workspace_on_success_and_partial_failure() {
    let source = Value::Datum(compressed(0, 9, &[8, b'a', b'b', b'c', 3, 3]));
    let token = CancellationToken::new();
    // Decompression fits, but the physical destination or its shared header does not.
    for limit in [9, 22] {
        let budget = MemoryBudget::new(limit);
        let control = ProductionControl::new(&budget, &token, &token);
        assert_eq!(
            copy_constant_field_with_control(&source, &ColumnType::Text, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(budget.peak(), limit);
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(4096);
    let control = ProductionControl::new(&budget, &token, &token);
    let output = copy_constant_field_with_control(&source, &ColumnType::Text, &control).unwrap();
    assert_eq!(budget.used(), output.reserved_bytes());
    assert!(budget.peak() > budget.used());
    let Value::Datum(datum) = &*output else {
        panic!("physical output")
    };
    assert_eq!(read(datum).unwrap(), Value::Str("abcabcabc".into()));
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn constant_field_copy_preserves_inline_values_and_checks_both_cancellation_scopes() {
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(0);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        for value in [Value::Null, Value::Int(7)] {
            let output =
                copy_constant_field_with_control(&value, &ColumnType::Integer, &control).unwrap();
            assert_eq!(*output, value);
            assert_eq!(output.reserved_bytes(), 0);
        }
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = copy_constant_field_with_control(&Value::Null, &ColumnType::Integer, &control)
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}
