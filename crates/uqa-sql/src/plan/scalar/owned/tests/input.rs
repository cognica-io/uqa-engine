//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{BinaryOp, FunctionBinding, FunctionDispatch, FunctionOrderSyntax},
    plan::ScalarInputWork as Work,
    SQLError, ScalarOrder,
};
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

type Input = ImageDecoder<OwnedScalarExpr, SQLError>;

fn source(call: bool) -> ScalarExpr {
    let items = vec![text(101), text(203), text(307)];
    if call {
        let mut binding = FunctionBinding::builtin_by_name("old", None);
        binding.argument_types = vec!["text".into(); 3];
        ScalarExpr::Func {
            name: "function 한🙂".repeat(400),
            binding: Some(binding),
            args: items,
            distinct: true,
            order_by: vec![ScalarOrder {
                expr: text(401),
                descending: true,
                nulls: Some(crate::ast::NullsOrder::Last),
            }],
            order_syntax: FunctionOrderSyntax::Legacy,
            filter: Some(Box::new(text(503))),
        }
    } else {
        ScalarExpr::Row(items)
    }
}

async fn construct<W: Work>(
    kind: usize,
    source: OwnedScalarExpr,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> std::result::Result<OwnedScalarExpr, W::Error>
where
    W::Error: From<CatalogRetentionError>,
{
    if kind >= 6 {
        let binding = Some(FunctionBinding::dispatched_with_control(
            FunctionDispatch::AnyOperator,
            control,
        )?);
        return if kind == 6 {
            source
                .replace_call_binding_from(binding, control, work)
                .await
        } else {
            source
                .replace_call_metadata_from(control.copy_text("renamed")?, binding, control, work)
                .await
        };
    }
    let mut children = source.into_children();
    match kind {
        0 => OwnedScalarExpr::array_from(children, control, work).await,
        1 => OwnedScalarExpr::row_from(children, control, work).await,
        2 | 3 => OwnedScalarExpr::boolean_from(children, kind == 2, control, work).await,
        4 => {
            let condition = children.next().unwrap();
            let value = children.next().unwrap();
            let fallback = children.next().unwrap();
            OwnedScalarExpr::case_from(
                None,
                [(condition, value)].into_iter(),
                Some(fallback),
                control,
                work,
            )
            .await
        }
        5 => {
            OwnedScalarExpr::call_from(
                control.copy_text("selected")?,
                None,
                children,
                control,
                work,
            )
            .await
        }
        _ => unreachable!(),
    }
}

fn input(
    kind: usize,
    source: OwnedScalarExpr,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        construct(kind, source, &control, &mut input).await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> std::result::Result<Option<OwnedScalarExpr>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn generated_scalar_inputs_preserve_fields_payload_addresses_and_each_cancelled_prefix() {
    for kind in 0..8 {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let source = source(kind >= 6);
        let ordinary = crate::catalog::work::inline(construct(
            kind,
            source.copy_for_rewrite(&control).unwrap(),
            &control,
            &mut (),
        ))
        .unwrap();
        let owned = source.copy_for_rewrite(&control).unwrap();
        let child = owned
            .expression
            .children(crate::ir::children::Scope::Construction)
            .next()
            .unwrap();
        let ScalarExpr::Literal(Value::Str(text)) = child else {
            panic!("text child")
        };
        let address = text.as_ptr();
        let mut reader = input(kind, owned, &native, &work, &token, &token);
        let mut polls = 0;
        let value = loop {
            polls += 1;
            assert!(polls < 10000);
            if let Some(value) = advance(&mut reader).unwrap() {
                break value;
            }
        };
        assert!(polls > 3);
        assert_eq!(*ordinary, *value);
        assert_eq!(ordinary.reserved_bytes(), value.reserved_bytes());
        let child = value
            .expression
            .children(crate::ir::children::Scope::Construction)
            .next()
            .unwrap();
        let ScalarExpr::Literal(Value::Str(text)) = child else {
            panic!("text child")
        };
        assert_eq!(text.as_ptr(), address);
        if kind >= 6 {
            let ScalarExpr::Func {
                name,
                binding,
                distinct,
                order_by,
                order_syntax,
                filter,
                ..
            } = &*value
            else {
                unreachable!()
            };
            assert!(distinct);
            assert_eq!(*order_syntax, FunctionOrderSyntax::Legacy);
            assert!(order_by[0].descending);
            assert_eq!(order_by[0].nulls, Some(crate::ast::NullsOrder::Last));
            assert!(filter.is_some());
            assert_eq!(
                binding.as_ref().unwrap().dispatch,
                Some(FunctionDispatch::AnyOperator)
            );
            if kind == 7 {
                assert_eq!(name, "renamed");
            }
        }
        drop((reader, ordinary, value));
        let sibling = native.reserve(37).unwrap();
        for prefix in 0..polls {
            for cancellation in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let control = ProductionControl::new(&native, &original, &invoking);
                let owned = source.copy_for_rewrite(&control).unwrap();
                let mut reader = input(kind, owned, &native, &work, &original, &invoking);
                for _ in 0..prefix {
                    assert!(advance(&mut reader).unwrap().is_none());
                }
                if cancellation != 0 {
                    if cancellation == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
                }
                let allocations = allocation_counter::measure(|| drop(reader));
                assert_eq!(allocations.count_total, 0);
                assert_eq!((native.used(), work.used()), (37, 0));
            }
        }
        drop(sibling);
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn generated_scalar_failure_retires_attached_deep_children_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for case in [false, true] {
                let native = MemoryBudget::new(32 << 20);
                let token = CancellationToken::new();
                let control = ProductionControl::new(&native, &token, &token);
                let deep = super::retirement::deep(&control);
                let right = ScalarExpr::Star.copy_for_rewrite(&control).unwrap();
                let fallback = ScalarExpr::Star.copy_for_rewrite(&control).unwrap();
                let mut slots = BudgetedVec::<Option<ScalarMemory>>::new(&native);
                slots.reserve(1).unwrap();
                let bytes =
                    slots.capacity() * size_of::<Option<ScalarMemory>>() + size_of::<ScalarExpr>();
                drop(slots);
                let held = native
                    .reserve(native.limit() - native.used() - bytes)
                    .unwrap();
                let result = if case {
                    OwnedScalarExpr::case(
                        Some(deep),
                        [(right, fallback)].into_iter(),
                        None,
                        &control,
                    )
                } else {
                    drop(fallback);
                    OwnedScalarExpr::binary(BinaryOp::Equal, deep, right, &control)
                };
                let error: SQLError = result.unwrap_err().into();
                assert_eq!(error.sqlstate(), Some("53200"));
                assert_eq!(native.used(), held.bytes());
                drop(held);
                assert_eq!(native.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn call_replacement_consumes_work_for_wide_children_and_old_binding_retirement() {
    let native = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let mut source = source(true);
    let ScalarExpr::Func { args, binding, .. } = &mut source else {
        unreachable!()
    };
    *args = (0..4096).map(ScalarExpr::Param).collect();
    binding.as_mut().unwrap().argument_types = vec!["text".into(); 4096];
    let source = source.copy_for_rewrite(&control).unwrap();
    let mut reader = input(7, source, &native, &work, &token, &token);
    let mut polls = 0;
    let output = loop {
        polls += 1;
        assert!(polls < 100_000);
        if let Some(output) = advance(&mut reader).unwrap() {
            break output;
        }
    };
    assert!(polls > 3 * 4096);
    let ScalarExpr::Func { args, .. } = &*output else {
        unreachable!()
    };
    assert_eq!(args.len(), 4096);
    for (index, expression) in args.iter().enumerate() {
        assert_eq!(*expression, ScalarExpr::Param(index));
    }
    drop((reader, output));
    assert_eq!((native.used(), work.used()), (0, 0));
}
