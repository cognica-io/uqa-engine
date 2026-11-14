//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{
        ColumnType, FrameExclusion, FrameMode, FunctionBinding, FunctionCallSyntax,
        FunctionDispatch, NullsOrder, WindowCallModifiers,
    },
    catalog::image::input::window_tests::{assert_retirement, read_windowed, WINDOWS},
    expr::composites::CompositeConstantSource,
    ir::{ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec},
};

fn fixture() -> ScalarExpr {
    let control = ProductionControl::uncontrolled();
    ScalarExpr::Array(vec![
        ScalarExpr::TypedLiteral {
            value: Value::Str("한λ😀".repeat(1025)),
            ty: "original.type".into(),
            collation_oid: Some(950),
            composite_source: Some(Box::new(CompositeConstantSource {
                value: Value::Bytes(vec![37; 4097]),
                descriptors: vec![],
            })),
            bound_type: Some(ColumnType::Text),
            parameter_index: Some(7),
        },
        ScalarExpr::Func {
            name: "original.function".into(),
            binding: Some(
                FunctionBinding::dispatched_with_control(FunctionDispatch::NamedArgument, &control)
                    .unwrap()
                    .into_uncontrolled()
                    .unwrap(),
            ),
            args: vec![ScalarExpr::Param(3)],
            distinct: true,
            order_by: vec![ScalarOrder {
                expr: ScalarExpr::ScalarSubquery(13),
                descending: true,
                nulls: Some(NullsOrder::Last),
            }],
            order_syntax: FunctionCallSyntax::WithinGroup,
            filter: None,
        },
        ScalarExpr::WindowCall {
            name: "original.window".into(),
            args: vec![ScalarExpr::Position(2)],
            spec: ScalarWindowSpec {
                definition: Some(17),
                partition_by: vec![ScalarExpr::Column("retained".into())],
                order_by: vec![ScalarOrder {
                    expr: ScalarExpr::ScalarSubquery(11),
                    descending: false,
                    nulls: Some(NullsOrder::First),
                }],
                frame: Some(ScalarWindowFrame {
                    mode: FrameMode::Groups,
                    start: ScalarFrameBound::Preceding(Box::new(ScalarExpr::Literal(Value::Int(
                        3,
                    )))),
                    end: ScalarFrameBound::Following(Box::new(ScalarExpr::Param(5))),
                    between: true,
                    exclusion: FrameExclusion::Ties,
                }),
            },
            filter: Some(Box::new(ScalarExpr::Exists {
                subquery: 19,
                negated: true,
            })),
            modifiers: WindowCallModifiers {
                distinct: true,
                ordered: true,
                within_group: true,
            },
        },
    ])
}

#[test]
fn native_scalar_input_preserves_bound_fields_and_scope_addresses_across_windows() {
    let source = fixture();
    let bytes = encode(&source);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let value = read_windowed(
            ScalarExpr::image_input(&memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(*value, source);
        assert_eq!(value.references.query, Some(19));
        assert_eq!(value.references.window, Some(17));
        assert_eq!(encode(&value), bytes);
        token.cancel();
        let full = memory.reserve(memory.available()).unwrap();
        assert_eq!(encode(&value), bytes);
        drop((value, full));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_scalar_input_retires_partial_frames_at_all_terminal_boundaries() {
    assert_retirement(&encode(&fixture()), ScalarExpr::image_input);
}

#[test]
fn native_scalar_input_keeps_original_literals_after_input_and_parent_retirement() {
    let source_memory = MemoryBudget::new(16 << 20);
    let source_token = CancellationToken::new();
    let control = ProductionControl::new(&source_memory, &source_token, &source_token);
    let text = "한λ😀".repeat(1025);
    let constant = RetainedConstantValue::from_produced(
        control.copy_value(&Value::Str(text.clone())).unwrap(),
        &control,
    )
    .unwrap();
    let literal = RetainedScalarLiteral::with_control(
        constant,
        Some(ColumnType::Text.clone_with_control(&control).unwrap()),
        Some(control.copy_text("original.domain").unwrap()),
        Some(950),
        &control,
    )
    .unwrap();
    let source = ScalarExpr::RetainedLiteral(literal);
    let bytes = encode(&source);
    drop(source);
    assert_eq!(source_memory.used(), 0);
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let value = read_windowed(
        ScalarExpr::image_input(&memory, &token).unwrap(),
        &bytes,
        1,
        3,
    );
    let ScalarExpr::RetainedLiteral(literal) = &*value else {
        panic!("retained literal");
    };
    let retained = literal.clone();
    drop(value);
    token.cancel();
    assert!(memory.used() >= text.len());
    assert_eq!(retained.value.read().unwrap(), Value::Str(text));
    assert_eq!(retained.declaration.as_deref(), Some("original.domain"));
    assert_eq!(retained.collation_oid, Some(950));
    drop(retained);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_scalar_input_retains_constant_stack_for_deep_original_trees() {
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
            let value = read_windowed(
                ScalarExpr::image_input(&memory, &token).unwrap(),
                &bytes,
                17,
                3,
            );
            assert_eq!(encode(&value), bytes);
            drop(value);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
