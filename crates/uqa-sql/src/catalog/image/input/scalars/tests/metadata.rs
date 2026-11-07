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
    catalog::collations::derivation::Derivation,
    expr::composites::CompositeConstantSource,
    ir::{ResultCollation, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec},
};

fn roundtrip(value: ScalarExpr) {
    let bytes = encode(&value);
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let restored = ScalarExpr::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert_eq!(*restored, value);
    assert_eq!(encode(&restored), bytes);
    drop(restored);
    assert_eq!(memory.used(), 0);
}

#[test]
fn executable_scalar_images_preserve_native_addresses_and_selected_collations() {
    for (tag, value) in [
        (24, ScalarExpr::Position(usize::MAX)),
        (26, ScalarExpr::ScalarSubquery(usize::MAX)),
    ] {
        let mut expected = image(&[tag]);
        expected.extend((usize::MAX as u64).to_le_bytes());
        assert_eq!(encode(&value), expected);
        roundtrip(value);
    }
    for negated in [false, true] {
        let value = ScalarExpr::InSubquery {
            expr: Box::new(ScalarExpr::Position(4)),
            subquery: 42,
            negated,
        };
        let mut expected = image(&[28]);
        expected.extend(42u64.to_le_bytes());
        expected.extend([u8::from(negated), 24]);
        expected.extend(4u64.to_le_bytes());
        assert_eq!(encode(&value), expected);
        roundtrip(value);
        roundtrip(ScalarExpr::Exists {
            subquery: 37,
            negated,
        });
    }
    for derivation in [
        Derivation::None,
        Derivation::Implicit(950),
        Derivation::Explicit(100),
        Derivation::Conflict(950, 951),
    ] {
        roundtrip(ScalarExpr::Collated {
            expr: Box::new(ScalarExpr::Column("retained.name".into())),
            ty: ColumnType::Text,
            collation: ResultCollation::new(derivation, 0),
        });
    }
}

#[test]
fn executable_scalar_images_keep_bound_literals_composite_sources_and_overload_selection() {
    roundtrip(ScalarExpr::CompositeRow {
        items: vec![ScalarExpr::Position(1), ScalarExpr::Position(2)],
        binding: crate::ast::CompositeRowBinding {
            ty: "original.row".into(),
            attributes: vec![2, 5],
            layout: Some(crate::ast::CompositeRowLayout {
                width: 7,
                implicit: false,
            }),
            argument_types: Some(vec![ColumnType::Integer, ColumnType::Text]),
        },
        bound_type: Some(ColumnType::Record),
    });
    roundtrip(ScalarExpr::TypedLiteral {
        value: Value::Array(
            uqa_core::ArrayValue::with_lower_bounds(vec![Value::Int(-37), Value::Null], vec![-4])
                .unwrap(),
        ),
        ty: "original.type[]".into(),
        collation_oid: Some(950),
        composite_source: Some(Box::new(CompositeConstantSource {
            value: Value::Bytes(vec![0, 255, 1]),
            descriptors: vec![],
        })),
        bound_type: Some(ColumnType::Array(Box::new(ColumnType::Integer))),
        parameter_index: Some(7),
    });
    let control = ProductionControl::uncontrolled();
    for syntax in [
        FunctionCallSyntax::Legacy,
        FunctionCallSyntax::Ordinary,
        FunctionCallSyntax::WithinGroup,
        FunctionCallSyntax::Extract,
    ] {
        roundtrip(ScalarExpr::Func {
            name: "selected.function".into(),
            binding: Some(
                FunctionBinding::dispatched_with_control(FunctionDispatch::NamedArgument, &control)
                    .unwrap()
                    .into_uncontrolled()
                    .unwrap(),
            ),
            args: vec![ScalarExpr::Position(1), ScalarExpr::Param(2)],
            distinct: true,
            order_by: vec![ScalarOrder {
                expr: ScalarExpr::ScalarSubquery(3),
                descending: true,
                nulls: Some(NullsOrder::Last),
            }],
            order_syntax: syntax,
            filter: Some(Box::new(ScalarExpr::Exists {
                subquery: 4,
                negated: true,
            })),
        });
    }
}

fn bound(tag: u8, position: usize) -> ScalarFrameBound {
    match tag {
        0 => ScalarFrameBound::UnboundedPreceding,
        1 => ScalarFrameBound::UnboundedFollowing,
        2 => ScalarFrameBound::CurrentRow,
        3 => ScalarFrameBound::Preceding(Box::new(ScalarExpr::Position(position))),
        _ => ScalarFrameBound::Following(Box::new(ScalarExpr::Position(position))),
    }
}

#[test]
fn executable_window_images_preserve_each_clause_child_order_and_written_flags() {
    for mode in [FrameMode::Rows, FrameMode::Range, FrameMode::Groups] {
        for exclusion in [
            FrameExclusion::NoOthers,
            FrameExclusion::CurrentRow,
            FrameExclusion::Group,
            FrameExclusion::Ties,
        ] {
            for start in 0..5 {
                for end in 0..5 {
                    roundtrip(ScalarExpr::WindowCall {
                        name: "window.function".into(),
                        args: vec![ScalarExpr::Position(1), ScalarExpr::Position(2)],
                        spec: ScalarWindowSpec {
                            definition: Some(31),
                            partition_by: vec![ScalarExpr::Position(3)],
                            order_by: vec![ScalarOrder {
                                expr: ScalarExpr::Position(4),
                                descending: true,
                                nulls: Some(NullsOrder::First),
                            }],
                            frame: Some(ScalarWindowFrame {
                                mode,
                                start: bound(start, 5),
                                end: bound(end, 6),
                                between: end % 2 == 0,
                                exclusion,
                            }),
                        },
                        filter: Some(Box::new(ScalarExpr::Position(7))),
                        modifiers: WindowCallModifiers {
                            distinct: true,
                            ordered: true,
                            within_group: true,
                        },
                    });
                }
            }
        }
    }
    roundtrip(ScalarExpr::WindowCall {
        name: "row_number".into(),
        args: vec![],
        spec: ScalarWindowSpec {
            definition: None,
            partition_by: vec![],
            order_by: vec![],
            frame: None,
        },
        filter: None,
        modifiers: WindowCallModifiers::default(),
    });
}

#[test]
fn executable_scalar_image_reader_borrows_payloads_and_terminates_after_failure() {
    let value = ScalarExpr::Literal(Value::Bytes(vec![51; 1 << 20]));
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let mut reader = None;
    let allocation =
        allocation_counter::measure(|| reader = Some(value.image_reader(&memory, &token).unwrap()));
    assert_eq!(allocation.count_total, 0);
    let mut reader = reader.unwrap();
    let mut output = vec![0; 32768];
    assert!(reader.read(&mut output).unwrap() <= 16384);
    token.cancel();
    assert_eq!(
        reader.read(&mut output).unwrap_err().sqlstate(),
        Some("57014")
    );
    assert_eq!(memory.used(), 0);
    assert!(reader.read(&mut output).is_err());
    assert!(memory.peak() < 32 << 10);
}

#[derive(Debug)]
struct Unreadable;
impl crate::ir::ConstantValueSource for Unreadable {
    fn read(&self, _: u64) -> Result<Value, SQLError> {
        panic!("image output must not invoke a retained source")
    }
}

#[test]
fn executable_scalar_images_reject_uncaptured_values_without_invoking_sources() {
    let value = ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
        RetainedConstantValue::new(std::sync::Arc::new(Unreadable), 9),
        None,
        None,
        None,
    ));
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(16 << 20);
    let mut reader = value.image_reader(&memory, &token).unwrap();
    assert!(reader.read(&mut [0; 64]).is_err());
    assert_eq!(memory.used(), 0);
    assert!(reader.read(&mut [0; 64]).is_err());
}

#[test]
fn executable_scalar_native_retirement_uses_no_new_allocation() {
    let mut bytes = image(&[]);
    for _ in 0..10000 {
        bytes.extend([13, 6]);
    }
    bytes.push(0);
    bytes.extend(std::iter::repeat_n(2, 10000));
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(64 << 20);
    let restored = ScalarExpr::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    let allocation = allocation_counter::measure(|| drop(restored));
    assert_eq!(allocation.count_total, 0);
    assert_eq!(memory.used(), 0);
}

#[test]
fn executable_scalar_retirement_handles_every_window_continuation_on_small_stacks() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let mut value = ScalarExpr::Column("original".into());
            for position in 0..3000 {
                if position % 7 == 6 {
                    value = ScalarExpr::Collated {
                        expr: Box::new(value),
                        ty: ColumnType::Text,
                        collation: ResultCollation::new(Derivation::Implicit(950), 950),
                    };
                    continue;
                }
                let mut spec = ScalarWindowSpec {
                    definition: None,
                    partition_by: vec![],
                    order_by: vec![],
                    frame: None,
                };
                let mut args = vec![];
                let mut filter = None;
                match position % 7 {
                    0 => args = vec![value, ScalarExpr::Position(position)],
                    1 => spec.partition_by = vec![value, ScalarExpr::Position(position)],
                    2 => {
                        spec.order_by = vec![ScalarOrder {
                            expr: value,
                            descending: true,
                            nulls: None,
                        }];
                    }
                    3 => {
                        spec.frame = Some(ScalarWindowFrame {
                            mode: FrameMode::Rows,
                            start: ScalarFrameBound::Preceding(Box::new(value)),
                            end: ScalarFrameBound::Following(Box::new(ScalarExpr::Position(
                                position,
                            ))),
                            between: true,
                            exclusion: FrameExclusion::Ties,
                        });
                    }
                    4 => {
                        spec.frame = Some(ScalarWindowFrame {
                            mode: FrameMode::Rows,
                            start: ScalarFrameBound::CurrentRow,
                            end: ScalarFrameBound::Following(Box::new(value)),
                            between: false,
                            exclusion: FrameExclusion::NoOthers,
                        });
                    }
                    _ => filter = Some(Box::new(value)),
                }
                value = ScalarExpr::WindowCall {
                    name: "original".into(),
                    args,
                    spec,
                    filter,
                    modifiers: WindowCallModifiers::default(),
                };
            }
            let bytes = encode(&value);
            crate::ir::ownership::retire(&mut value);
            let memory = MemoryBudget::new(64 << 20);
            let token = CancellationToken::new();
            let restored = ScalarExpr::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
            assert_eq!(encode(&restored), bytes);
            let allocation = allocation_counter::measure(|| drop(restored));
            assert_eq!(allocation.count_total, 0);
            assert_eq!(memory.used(), 0);
            assert!(
                ScalarExpr::read_image(&mut &bytes[..bytes.len() - 1], &memory, &token).is_err()
            );
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
