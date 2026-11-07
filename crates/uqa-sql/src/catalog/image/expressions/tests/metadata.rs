//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{CompositeRowBinding, CompositeRowLayout, RetainedInputLiteral},
    expr::composites::{
        CompositeAttribute, CompositeConstantSource, CompositeTypeDescriptor,
        DroppedCompositeAttribute,
    },
    ir::{RetainedConstantValue, RetainedScalarLiteral},
};
use uqa_core::memory::ProductionControl;

#[test]
fn composite_row_images_preserve_layout_positions_and_argument_declarations() {
    let value = Expr::CompositeRow {
        items: vec![Expr::Default, Expr::Star],
        binding: CompositeRowBinding {
            ty: "old.row".into(),
            attributes: vec![3, 1],
            layout: Some(CompositeRowLayout {
                width: 4,
                implicit: true,
            }),
            argument_types: Some(vec![
                ColumnType::Text,
                ColumnType::Array(Box::new(ColumnType::Integer)),
            ]),
        },
    };
    let mut expected = vec![12];
    expected.extend(2u64.to_le_bytes());
    expected.extend(text("old.row"));
    expected.extend(2u64.to_le_bytes());
    expected.push(1);
    expected.extend(4i16.to_le_bytes());
    expected.extend([1, 1]);
    expected.extend(2u64.to_le_bytes());
    expected.extend(3i16.to_le_bytes());
    expected.extend(1i16.to_le_bytes());
    expected.extend([7, 44, 1, 2, 0]);
    assert_eq!(encode(&value), image(&expected));
    let value = Expr::CompositeRow {
        items: vec![],
        binding: CompositeRowBinding {
            ty: "t".into(),
            attributes: vec![],
            layout: None,
            argument_types: None,
        },
    };
    let mut expected = vec![12];
    expected.extend(0u64.to_le_bytes());
    expected.extend(text("t"));
    expected.extend(0u64.to_le_bytes());
    expected.extend([0, 0]);
    assert_eq!(encode(&value), image(&expected));
}

#[test]
fn typed_literal_images_preserve_original_composite_source_and_dropped_layout() {
    let value = Expr::TypedLiteral {
        value: Value::Float(f64::from_bits(0x8000_0000_0000_0000)),
        ty: "old.row".into(),
        collation_oid: Some(0),
        composite_source: Some(Box::new(CompositeConstantSource {
            value: Value::Bytes(vec![0, 0xff, 17]),
            descriptors: vec![CompositeTypeDescriptor {
                type_oid: 65536,
                relation_oid: 65537,
                attributes: vec![CompositeAttribute {
                    name: "original".into(),
                    ty: ColumnType::Varchar(Some(13)),
                    collation_oid: Some(950),
                    number: 2,
                }],
                dropped: vec![DroppedCompositeAttribute {
                    number: 1,
                    length: -1,
                    alignment: b'i',
                }],
            }],
        })),
    };
    let mut expected = vec![7];
    expected.extend(text("old.row"));
    expected.push(1);
    expected.extend(0u32.to_le_bytes());
    expected.push(1);
    expected.extend(value_image(&Value::Float(-0.0)));
    expected.extend(1u64.to_le_bytes());
    expected.extend(value_image(&Value::Bytes(vec![0, 0xff, 17])));
    expected.extend(65536u32.to_le_bytes());
    expected.extend(65537u32.to_le_bytes());
    expected.extend(1u64.to_le_bytes());
    expected.extend(1u64.to_le_bytes());
    expected.extend(text("original"));
    expected.push(1);
    expected.extend(950u32.to_le_bytes());
    expected.extend(2i16.to_le_bytes());
    expected.extend([39, 1]);
    expected.extend(13u32.to_le_bytes());
    expected.extend(1i16.to_le_bytes());
    expected.extend((-1i64).to_le_bytes());
    expected.push(b'i');
    assert_eq!(encode(&value), image(&expected));
}

#[test]
fn captured_retained_literal_images_keep_native_type_and_declaration_independent() {
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let value = RetainedConstantValue::from_produced(
        control.copy_value(&Value::Int(17)).unwrap(),
        &control,
    )
    .unwrap();
    let value = Expr::RetainedLiteral(Box::new(RetainedInputLiteral(
        RetainedScalarLiteral::with_control(
            value,
            Some(ColumnType::Integer.clone_with_control(&control).unwrap()),
            Some(control.copy_text("old.domain").unwrap()),
            Some(950),
            &control,
        )
        .unwrap(),
    )));
    let retained = memory.used();
    let mut expected = vec![23, 1];
    expected.extend(text("old.domain"));
    expected.push(1);
    expected.extend(950u32.to_le_bytes());
    expected.extend([1, 1]);
    expected.extend(value_image(&Value::Int(17)));
    assert_eq!(encode(&value), image(&expected));
    assert_eq!(memory.used(), retained);
    drop(value);
    assert_eq!(memory.used(), 0);
}
