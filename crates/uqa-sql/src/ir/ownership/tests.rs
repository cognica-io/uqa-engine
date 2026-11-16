//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{
        ColumnType, CompositeFieldBinding, CompositeRowBinding, FunctionBinding,
        FunctionOrderSyntax, RoutineInvocationBinding, RoutineVariadicMode,
    },
    catalog::collations::derivation::Derivation,
    expr::composites::{CompositeAttribute, CompositeConstantSource, CompositeTypeDescriptor},
    ir::ResultCollation,
    SQLError,
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, MemoryReservation, ProductionControl},
    CancellationToken, Value,
};

fn ty(depth: usize) -> ColumnType {
    let mut value = ColumnType::Text;
    for index in 0..depth {
        value = if index % 2 == 0 {
            ColumnType::Array(Box::new(value))
        } else {
            ColumnType::Domain {
                schema: "public".into(),
                name: "nested".into(),
                oid: 65_536,
                array_oid: None,
                collation_oid: None,
                base: Box::new(value),
            }
        };
    }
    value
}

fn expression(kind: usize, depth: usize, width: usize) -> ScalarExpr {
    match kind {
        0 => {
            let mut binding = FunctionBinding::builtin_by_name("f", None);
            binding.argument_types = vec!["text".into(); width];
            binding.invocation = Some(Box::new(RoutineInvocationBinding {
                argument_positions: (0..width).collect(),
                argument_targets: vec!["text".into(); width],
                argument_sources: (0..width)
                    .map(|index| (index % 2 == 0).then(|| "text".into()))
                    .collect(),
                parameter_types: vec!["text".into(); width],
                return_type: Some("text".into()),
                variadic_mode: RoutineVariadicMode::None,
            }));
            binding.set_composite_field(CompositeFieldBinding {
                type_oid: 65_536,
                number: 1,
                result_type: ty(depth),
                collation_oid: None,
                dropped: false,
                changed_type: Some(ty(depth)),
            });
            ScalarExpr::Func {
                name: "f".into(),
                binding: Some(binding),
                args: Vec::new(),
                distinct: false,
                order_by: Vec::new(),
                order_syntax: FunctionOrderSyntax::Legacy,
                filter: None,
            }
        }
        1 => ScalarExpr::CompositeRow {
            items: vec![ScalarExpr::Literal(Value::Int(1))],
            binding: CompositeRowBinding {
                ty: "public.record".into(),
                attributes: vec![1],
                layout: None,
                argument_types: Some(vec![ty(depth), ty(depth)]),
            },
            bound_type: Some(ty(depth)),
        },
        2 => ScalarExpr::TypedLiteral {
            value: Value::Int(1),
            ty: "text".into(),
            collation_oid: None,
            composite_source: Some(Box::new(CompositeConstantSource {
                value: Value::Null,
                descriptors: vec![
                    CompositeTypeDescriptor {
                        type_oid: 65_536,
                        relation_oid: 65_537,
                        dropped: Vec::new(),
                        attributes: (0..width)
                            .map(|index| CompositeAttribute {
                                name: format!("field_{index}"),
                                ty: ColumnType::Text,
                                collation_oid: None,
                                number: i16::try_from(index + 1).unwrap(),
                            })
                            .collect(),
                    },
                    CompositeTypeDescriptor {
                        type_oid: 65_538,
                        relation_oid: 65_539,
                        dropped: Vec::new(),
                        attributes: vec![CompositeAttribute {
                            name: "deep_field".into(),
                            ty: ty(depth),
                            collation_oid: None,
                            number: 1,
                        }],
                    },
                ],
            })),
            bound_type: Some(ty(depth)),
            parameter_index: None,
        },
        3 => ScalarExpr::Collated {
            expr: Box::new(ScalarExpr::Literal(Value::Int(1))),
            ty: ty(depth),
            collation: ResultCollation::new(Derivation::None, 0),
        },
        _ => unreachable!(),
    }
}

#[test]
fn scalar_retirement_bounds_private_type_and_signature_metadata_work() {
    for kind in 0..4 {
        let mut retirement = Retirement::new(expression(kind, 4096, 2048));
        let mut complete = false;
        let mut steps = 0;
        while !complete {
            let freed = allocation_counter::measure(|| complete = retirement.advance());
            assert_eq!(freed.count_total, 0);
            // Each transition releases one type wrapper or signature element. The final fixed-size node can release its remaining primitive fields and empty buffers.
            assert!(freed.count_current >= -16, "{kind}: {freed:?}");
            steps += 1;
            assert!(steps < 30_000);
        }
        assert!(steps > 4096);
        if kind == 0 {
            assert!(steps >= 2 * 4096 + 4 * 2048);
        }
        assert_eq!(
            allocation_counter::measure(|| drop(retirement)).count_total,
            0
        );
    }
}

#[test]
fn scalar_metadata_retirement_and_ordinary_cleanup_share_small_stack_ownership() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for kind in 0..4 {
                for stepped in [false, true] {
                    let mut value = expression(kind, 8192, 32);
                    let released = allocation_counter::measure(|| {
                        if stepped {
                            let mut retirement = Retirement::new(value);
                            for _ in 0..1000 {
                                assert!(!retirement.advance());
                            }
                            drop(retirement);
                        } else {
                            retire(&mut value);
                            assert!(matches!(value, ScalarExpr::Star));
                        }
                    });
                    assert_eq!(released.count_total, 0);
                    assert!(released.count_current < -8192);
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

struct OwnedRetirement {
    retirement: Retirement,
    _memory: Option<MemoryReservation>,
}

impl OwnedRetirement {
    fn advance(&mut self) -> bool {
        self.retirement.advance()
    }
}

fn input(
    value: &ScalarExpr,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<(), SQLError> {
    let control = ProductionControl::new(native, original, invoking);
    let (value, memory) = value.clone_with_control(&control).unwrap().into_parts();
    let mut owned = OwnedRetirement {
        retirement: Retirement::new(value),
        _memory: memory,
    };
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&native, &original, work.cancellation);
        loop {
            control.check()?;
            work.checkpoint().await?;
            control.check()?;
            if owned.advance() {
                return Ok(());
            }
        }
    })
    .unwrap()
}

fn advance(input: &mut ImageDecoder<(), SQLError>) -> Result<Option<()>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn scalar_metadata_retirement_keeps_leases_through_every_interruption() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let sibling = native.reserve(37).unwrap();
    for kind in 0..4 {
        let source = expression(kind, 7, 3);
        let mut prefixes = 0;
        for prefix in 0..256 {
            let mut complete = false;
            for cancellation in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let mut input = input(&source, &native, &work, &original, &invoking);
                let retained = native.used();
                assert!(retained > sibling.bytes());
                let exhausted = native.reserve(native.limit() - retained).unwrap();
                for _ in 0..prefix {
                    let mut result = None;
                    assert_eq!(
                        allocation_counter::measure(|| result = advance(&mut input).unwrap())
                            .count_total,
                        0
                    );
                    if result.is_some() {
                        complete = true;
                        break;
                    }
                    assert_eq!(native.used(), native.limit());
                }
                if !complete && cancellation != 0 {
                    if cancellation == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert_eq!(advance(&mut input).unwrap_err().sqlstate(), Some("57014"));
                }
                assert_eq!(allocation_counter::measure(|| drop(input)).count_total, 0);
                drop(exhausted);
                assert_eq!((native.used(), work.used()), (sibling.bytes(), 0));
            }
            prefixes += 1;
            if complete {
                break;
            }
        }
        assert!(prefixes > 7 && prefixes < 256);
        drop(crate::plan::OwnedScalarExpr::uncontrolled(source));
    }
}
