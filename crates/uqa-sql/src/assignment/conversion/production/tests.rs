//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn physical_identity_assignment_keeps_unread_payloads_and_original_allowances() {
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for ty in [
        ColumnType::Boolean,
        ColumnType::Bytea,
        ColumnType::Array(Box::new(ColumnType::Integer)),
    ] {
        let original = Value::Datum(uqa_core::DatumValue::new(
            crate::catalog::type_metadata::pg_type_oid(&ty) as u32,
            0,
            vec![],
        ));
        let input = control.copy_value(&original).unwrap();
        let bytes = input.reserved_bytes();
        let output =
            convert_value_to_column_type_from_with_control(input, &ty, Some(&ty), &control)
                .unwrap();
        assert!(output.has_same_representation(&original));
        assert_eq!(output.reserved_bytes(), bytes);
        assert_eq!(budget.used(), bytes);
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn physical_builtin_conversion_reads_the_selected_value_under_its_allowance() {
    let budget = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let array = Value::Array(
        ArrayValue::with_lower_bounds(vec![Value::Int(7), Value::Int(9)], vec![-2]).unwrap(),
    );
    for (value, source, target) in [
        (Value::Bool(true), ColumnType::Boolean, ColumnType::Boolean),
        (
            Value::Bytes(vec![0, 1, 255]),
            ColumnType::Bytea,
            ColumnType::Bytea,
        ),
        (
            array,
            ColumnType::Array(Box::new(ColumnType::Integer)),
            ColumnType::Array(Box::new(ColumnType::BigInteger)),
        ),
    ] {
        let encoded =
            crate::expr::datums::encoding::encode(&value, &source, None, &control).unwrap();
        let mut bytes = Vec::new();
        if crate::catalog::type_metadata::pg_type_len(&source) == -1 {
            bytes.extend_from_slice(&(((encoded.len() as u32) + 4) * 4).to_le_bytes());
        }
        bytes.extend_from_slice(&encoded);
        drop(encoded);
        let physical = Value::Datum(uqa_core::DatumValue::new(
            crate::catalog::type_metadata::pg_type_oid(&source) as u32,
            0,
            bytes,
        ));
        let input = control.copy_value(&physical).unwrap();
        let input_bytes = input.reserved_bytes();
        let output = convert_value_to_column_type_with_control(input, &target, &control).unwrap();
        assert_eq!(*output, value);
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
        if matches!(value, Value::Bool(_)) {
            continue;
        }
        let budget = MemoryBudget::new(input_bytes);
        let rejected = ProductionControl::new(&budget, &token, &token);
        let input = rejected.copy_value(&physical).unwrap();
        assert_eq!(
            convert_value_to_column_type_with_control(input, &target, &rejected)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn typed_node_tree_assignments_keep_the_admitted_value_and_lease() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let expected = Value::Str("{CONST :consttype 23}".into());
    let input = control.copy_value(&expected).unwrap();
    let bytes = input.reserved_bytes();
    let result = convert_value_to_column_type_from_with_control(
        input,
        &ColumnType::PgNodeTree,
        Some(&ColumnType::PgNodeTree),
        &control,
    )
    .unwrap();
    assert_eq!(*result, expected);
    assert_eq!(result.reserved_bytes(), bytes);
    assert_eq!(budget.used(), bytes);
    drop(result);
    assert_eq!(budget.used(), 0);
    let input = control.copy_value(&expected).unwrap();
    let error = convert_value_to_column_type_from_with_control(
        input,
        &ColumnType::PgNodeTree,
        None,
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn typed_byte_assignments_preserve_every_carrier_and_its_original_lease() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for byte in 0..=u8::MAX {
        let value = crate::expr::internal_char::carrier_with_control(byte, &control).unwrap();
        let original = value.reserved_bytes();
        let expected = (*value).clone();
        let output = convert_value_to_column_type_from_with_control(
            value,
            &ColumnType::InternalChar,
            Some(&ColumnType::InternalChar),
            &control,
        )
        .unwrap();
        assert_eq!(*output, expected);
        assert_eq!(output.reserved_bytes(), original);
        drop(output);
        assert_eq!(budget.used(), 0);
    }
    let input = control.copy_value(&Value::Int(-1)).unwrap();
    let output = convert_value_to_column_type_from_with_control(
        input,
        &ColumnType::InternalChar,
        Some(&ColumnType::Integer),
        &control,
    )
    .unwrap();
    assert_eq!(*output, Value::Str("ÿ".into()));
    drop(output);
    assert_eq!(budget.used(), 0);
    token.cancel();
    let value = ProductionControl::uncontrolled()
        .finish(Value::Str("ÿ".into()), None)
        .unwrap();
    assert_eq!(
        convert_value_to_column_type_from_with_control(
            value,
            &ColumnType::InternalChar,
            Some(&ColumnType::InternalChar),
            &control,
        )
        .unwrap_err()
        .sqlstate(),
        Some("57014")
    );
}

#[test]
fn controlled_assignment_preserves_character_array_numeric_and_vector_results() {
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let cases = [
        (
            Value::Str(String::new()),
            ColumnType::InternalChar,
            Value::Str(String::new()),
        ),
        (
            Value::Str("é".into()),
            ColumnType::InternalChar,
            Value::Str("Ã".into()),
        ),
        (
            Value::Str("\\377".into()),
            ColumnType::InternalChar,
            Value::Str("ÿ".into()),
        ),
        (
            Value::Str("é  ".into()),
            ColumnType::Varchar(Some(1)),
            Value::Str("é".into()),
        ),
        (
            Value::Str("é".into()),
            ColumnType::Character(3),
            Value::FixedChar("é  ".into()),
        ),
        (
            Value::Str("5".into()),
            ColumnType::Domain {
                collation_oid: None,
                schema: "public".into(),
                name: "number".into(),
                oid: 12,
                array_oid: None,
                base: Box::new(ColumnType::Integer),
            },
            Value::Int(5),
        ),
        (
            Value::Str("-12.345".into()),
            ColumnType::Numeric {
                precision: Some(4),
                scale: Some(2),
            },
            Value::Decimal(DecimalValue::parse("-12.35").unwrap()),
        ),
        (
            Value::List(vec![Value::Int(1), Value::Float(2.5)]),
            ColumnType::Vector(2),
            Value::List(vec![Value::Float(1.0), Value::Float(2.5)]),
        ),
        (
            Value::Array(
                ArrayValue::with_lower_bounds(
                    vec![Value::Str("2".into()), Value::Str("3".into())],
                    vec![-2],
                )
                .unwrap(),
            ),
            ColumnType::Array(Box::new(ColumnType::Integer)),
            Value::Array(
                ArrayValue::with_lower_bounds(vec![Value::Int(2), Value::Int(3)], vec![-2])
                    .unwrap(),
            ),
        ),
    ];
    for (source, ty, expected) in cases {
        let source = control.copy_value(&source).unwrap();
        let result = convert_value_to_column_type_with_control(source, &ty, &control).unwrap();
        assert_eq!(&*result, &expected);
        assert_eq!(budget.used(), result.reserved_bytes());
        drop(result);
        assert_eq!(budget.used(), 0);
    }
    let invalid = control.copy_value(&Value::Str("long".into())).unwrap();
    let error =
        convert_value_to_column_type_with_control(invalid, &ColumnType::Varchar(Some(1)), &control)
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22001"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn record_assignment_moves_existing_payloads_and_drops_the_source_buffer() {
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let source = control
        .copy_value(&Value::Row(
            vec![Value::Str("moved".into()), Value::Int(7)].into(),
        ))
        .unwrap();
    let Value::Row(source_values) = &*source else {
        unreachable!();
    };
    let Value::Str(source_text) = &source_values[0] else {
        unreachable!();
    };
    let pointer = source_text.as_ptr();
    let result =
        convert_value_to_column_type_with_control(source, &ColumnType::Record, &control).unwrap();
    let Value::Record(fields) = &*result else {
        unreachable!();
    };
    let Value::Str(text) = &fields.values()[0] else {
        unreachable!();
    };
    assert_eq!(text.as_ptr(), pointer);
    assert_eq!(fields.descriptor().field_names()[0], "f1");
    assert_eq!(fields.get(1), Some((&"f2".into(), &Value::Int(7))));
    let exact = fields.retained_buffer_bytes().unwrap()
        + fields
            .iter()
            .map(|(name, _)| name.capacity())
            .sum::<usize>()
        + text.capacity();
    assert_eq!(result.reserved_bytes(), exact);
    assert_eq!(budget.used(), exact);
    drop(result);
    assert_eq!(budget.used(), 0);
}

#[test]
fn failed_assignment_releases_partial_values_and_preserves_cancellation_kind() {
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let source = control.copy_value(&Value::Str("held".into())).unwrap();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error =
            convert_value_to_column_type_with_control(source, &ColumnType::Character(20), &control)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(128);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let source = control.copy_value(&Value::Str("held".into())).unwrap();
    let error =
        convert_value_to_column_type_with_control(source, &ColumnType::Character(4096), &control)
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
}
