//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{
        ColumnType, CompositeRowBinding, CompositeRowLayout, FunctionCallSyntax, NullsOrder,
        OrderBy,
    },
    catalog::image::input::window_tests::{assert_retirement, read_windowed, WINDOWS},
    expr::composites::{
        CompositeAttribute, CompositeConstantSource, CompositeTypeDescriptor,
        DroppedCompositeAttribute,
    },
};

fn metadata_fixture() -> Expr {
    let text = "한🙂".repeat(1025);
    Expr::Array(vec![
        Expr::Column(text.clone().into()),
        Expr::TypedLiteral {
            value: Value::Str(text.clone()),
            ty: "original.record".into(),
            collation_oid: Some(950),
            composite_source: Some(Box::new(CompositeConstantSource {
                value: Value::Bytes(vec![37; 8193]),
                descriptors: vec![CompositeTypeDescriptor {
                    type_oid: 65536,
                    relation_oid: 65537,
                    attributes: vec![CompositeAttribute {
                        name: text,
                        ty: ColumnType::Text,
                        collation_oid: Some(950),
                        number: 2,
                    }],
                    dropped: vec![DroppedCompositeAttribute {
                        number: 1,
                        length: 4,
                        alignment: b'i',
                    }],
                }],
            })),
        },
        Expr::CompositeRow {
            items: vec![Expr::Star, Expr::Default],
            binding: CompositeRowBinding {
                ty: "original.record".into(),
                attributes: vec![2, 4],
                layout: Some(CompositeRowLayout {
                    width: 4,
                    implicit: false,
                }),
                argument_types: Some(vec![ColumnType::Integer, ColumnType::Text]),
            },
        },
        Expr::Func {
            name: "original".into(),
            binding: None,
            distinct: true,
            order_syntax: FunctionCallSyntax::WithinGroup,
            filter: Some(Box::new(Expr::IsNull {
                expr: Box::new(Expr::Default),
                negated: true,
            })),
            args: vec![Expr::Literal(Value::Int(17))],
            order_by: vec![OrderBy {
                expr: Expr::Column("order".into()),
                descending: true,
                nulls: Some(NullsOrder::Last),
            }],
        },
        Expr::Case {
            base: Some(Box::new(Expr::Star)),
            when: vec![(Expr::Default, Expr::Column("then".into()))],
            else_branch: Some(Box::new(Expr::Literal(Value::Null))),
        },
    ])
}

#[test]
fn native_expression_input_bounds_partial_utf8_metadata_and_ordered_children() {
    let source = metadata_fixture();
    let expected = encode(&source);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let value = read_windowed(
            Expr::image_input(&memory, &token).unwrap(),
            &expected,
            width,
            steps,
        );
        assert_eq!(*value, source);
        assert_eq!(encode(&value), expected);
        token.cancel();
        let full = memory.reserve(memory.available()).unwrap();
        assert_eq!(encode(&value), expected);
        drop((value, full));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_expression_input_retires_partial_metadata_on_every_terminal_path() {
    assert_retirement(&encode(&metadata_fixture()), Expr::image_input);
}

#[test]
fn native_expression_input_preserves_retained_literals_after_parent_retirement() {
    let source_memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&source_memory, &token, &token);
    let text = "한🙂".repeat(1025);
    let constant = RetainedConstantValue::from_produced(
        control.copy_value(&Value::Str(text.clone())).unwrap(),
        &control,
    )
    .unwrap();
    let literal = RetainedScalarLiteral::with_control(
        constant,
        Some(ColumnType::Text.clone_with_control(&control).unwrap()),
        Some(control.copy_text("old.domain").unwrap()),
        Some(950),
        &control,
    )
    .unwrap();
    let source = Expr::RetainedLiteral(Box::new(RetainedInputLiteral(literal)));
    let expected = encode(&source);
    drop(source);
    assert_eq!(source_memory.used(), 0);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(16 << 20);
        let input_token = CancellationToken::new();
        let value = read_windowed(
            Expr::image_input(&memory, &input_token).unwrap(),
            &expected,
            width,
            steps,
        );
        let Expr::RetainedLiteral(literal) = &*value else {
            panic!("native retained literal")
        };
        let retained = literal.0.clone();
        assert_eq!(encode(&value), expected);
        drop(value);
        input_token.cancel();
        assert!(memory.used() >= text.len());
        assert_eq!(retained.value.read().unwrap(), Value::Str(text.clone()));
        assert_eq!(retained.declaration.as_deref(), Some("old.domain"));
        assert_eq!(retained.collation_oid, Some(950));
        drop(retained);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_expression_input_resumes_and_retires_deep_trees_on_small_stacks() {
    let depth = 10000;
    let mut bytes = image(&[]);
    for _ in 0..depth {
        bytes.extend([13, 6]);
    }
    bytes.push(0);
    bytes.extend(std::iter::repeat_n(2, depth));
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(move || {
            let memory = MemoryBudget::new(64 << 20);
            let token = CancellationToken::new();
            let value = read_windowed(Expr::image_input(&memory, &token).unwrap(), &bytes, 17, 1);
            assert_eq!(encode(&value), bytes);
            drop(value);
            assert_eq!(memory.used(), 0);
            let mut input = Expr::image_input(&memory, &token).unwrap();
            let mut reader = bytes.as_slice();
            assert!(input
                .advance(
                    &mut reader,
                    uqa_core::image_io::ImageDecodeWindow {
                        bytes: 16 + depth,
                        steps: usize::MAX
                    }
                )
                .unwrap()
                .is_none());
            drop(input);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
