//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{plan::ExpressionPlan, SQLError};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken, Value,
};

mod normalization;

type Input = ImageDecoder<Produced<ScalarWindowSpec>, SQLError>;

fn specification(source: &OwnedScalarExpr) -> &ScalarWindowSpec {
    let ScalarExpr::WindowCall { spec, .. } = &**source else {
        panic!("window call")
    };
    spec
}

fn input(
    source: Arc<OwnedScalarExpr>,
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
        let spec = specification(&source);
        OwnedWindowSpec::copy_parts_from(
            spec.definition,
            &spec.partition_by,
            &spec.order_by,
            spec.frame.as_ref(),
            &control,
            &mut input,
        )
        .await?
        .finish_from(&control, &mut input)
        .await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> std::result::Result<Option<Produced<ScalarWindowSpec>>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn fixture() -> Arc<OwnedScalarExpr> {
    let crate::Statement::Select(mut query) = crate::compile("SELECT sum(a) FILTER (WHERE b) OVER (PARTITION BY concat(c, '한🙂') ORDER BY d DESC NULLS LAST ROWS BETWEEN 2 PRECEDING AND 1 FOLLOWING)").unwrap().remove(0) else { panic!("SELECT") };
    Arc::new(OwnedScalarExpr {
        expression: ExpressionPlan::lower(query.projections.remove(0).expr).scalar,
        memory: None,
    })
}

#[test]
fn retained_window_copy_preserves_fields_and_retires_each_pending_prefix() {
    let source = fixture();
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let spec = specification(&source);
    let ordinary = OwnedWindowSpec::copy_parts(
        spec.definition,
        &spec.partition_by,
        &spec.order_by,
        spec.frame.as_ref(),
        &control,
    )
    .unwrap()
    .finish(&control)
    .unwrap();
    let mut reader = input(source.clone(), &native, &work, &token, &token);
    let mut polls = 0;
    let value = loop {
        polls += 1;
        assert!(polls < 10000);
        if let Some(value) = advance(&mut reader).unwrap() {
            break value;
        }
    };
    assert!(polls > 10);
    assert_eq!(*value, *spec);
    assert_eq!(*ordinary, *value);
    assert_eq!(ordinary.reserved_bytes(), value.reserved_bytes());
    drop((reader, ordinary, value));
    let sibling = native.reserve(29).unwrap();
    for prefix in 0..polls - 1 {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(source.clone(), &native, &work, &original, &invoking);
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
            let allocation = allocation_counter::measure(|| drop(reader));
            assert_eq!(allocation.count_total, 0);
            assert_eq!((native.used(), work.used()), (29, 0));
        }
    }
    drop(sibling);
    assert_eq!(native.used(), 0);
}

#[test]
fn window_copy_and_replacement_retire_deep_specifications_with_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut source = fixture();
            let ScalarExpr::WindowCall { spec, .. } =
                &mut Arc::get_mut(&mut source).unwrap().expression
            else {
                unreachable!()
            };
            let mut deep = ScalarExpr::Literal(Value::Str("leaf".into()));
            for _ in 0..8192 {
                deep = ScalarExpr::Not(Box::new(deep));
            }
            spec.partition_by = vec![deep];
            spec.order_by[0].expr = ScalarExpr::Literal(Value::Str("x".repeat(32 << 20)));
            let native = MemoryBudget::new(16 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let spec = specification(&source);
            let ordinary = OwnedWindowSpec::copy_parts(
                spec.definition,
                &spec.partition_by,
                &spec.order_by,
                spec.frame.as_ref(),
                &control,
            )
            .err()
            .expect("late ordering failure");
            let CatalogRetentionError::Memory(uqa_core::memory::MemoryError::Limit {
                required,
                ..
            }) = ordinary
            else {
                panic!("admission error")
            };
            assert!(
                required >= 32 << 20,
                "must complete the deep partition before ordering admission"
            );
            let resumed = input(source.clone(), &native, &work, &token, &token)
                .read(&mut std::io::empty())
                .unwrap_err();
            assert_eq!(resumed.to_string(), SQLError::from(ordinary).to_string());
            assert_eq!((native.used(), work.used()), (0, 0));
            // A complete old specification also retires iteratively when replaced.
            let ScalarExpr::WindowCall { spec, .. } =
                &mut Arc::get_mut(&mut source).unwrap().expression
            else {
                unreachable!()
            };
            spec.order_by.clear();
            let owned = source.copy_for_rewrite(&control).unwrap();
            let replacement = fixture();
            let owned = owned
                .replace_window_spec(specification(&replacement), &control)
                .unwrap();
            assert_eq!(specification(&owned), specification(&replacement));
            drop(owned);
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn retained_window_publication_retires_interrupted_deep_metadata_folds() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut source = fixture();
            let ScalarExpr::WindowCall { spec, .. } =
                &mut Arc::get_mut(&mut source).unwrap().expression
            else {
                unreachable!()
            };
            let mut deep = ScalarExpr::Literal(Value::Str("leaf".into()));
            for _ in 0..8192 {
                deep = ScalarExpr::Not(Box::new(deep));
            }
            spec.partition_by = vec![deep];
            let native = MemoryBudget::new(16 << 20);
            let work = MemoryBudget::new(1 << 20);
            let sibling = native.reserve(29).unwrap();
            for prefix in [0, 1, 2, 8, 1024, 8192, 16_384] {
                for cancellation in 0..3 {
                    let original = CancellationToken::new();
                    let invoking = CancellationToken::new();
                    let control = ProductionControl::new(&native, &original, &invoking);
                    let spec = specification(&source);
                    let spec = OwnedWindowSpec::copy_parts(
                        spec.definition,
                        &spec.partition_by,
                        &spec.order_by,
                        spec.frame.as_ref(),
                        &control,
                    )
                    .unwrap();
                    let budget = native.clone();
                    let retained = original.clone();
                    let mut reader: ImageDecoder<(), SQLError> =
                        ImageDecoder::new(&work, &invoking, |mut reader| async move {
                            let mut input = reader.input();
                            let control =
                                ProductionControl::new(&budget, &retained, input.cancellation);
                            let (value, memory) =
                                spec.finish_from(&control, &mut input).await?.into_parts();
                            drop(Pending::new(value));
                            drop(memory);
                            Ok(())
                        })
                        .unwrap();
                    for _ in 0..prefix {
                        assert!(reader
                            .advance(
                                &mut std::io::empty(),
                                ImageDecodeWindow { bytes: 1, steps: 1 }
                            )
                            .unwrap()
                            .is_none());
                    }
                    if cancellation != 0 {
                        if cancellation == 1 {
                            original.cancel();
                        } else {
                            invoking.cancel();
                        }
                        assert_eq!(
                            reader
                                .advance(
                                    &mut std::io::empty(),
                                    ImageDecodeWindow { bytes: 1, steps: 1 }
                                )
                                .unwrap_err()
                                .sqlstate(),
                            Some("57014")
                        );
                    }
                    let allocation = allocation_counter::measure(|| drop(reader));
                    assert_eq!(allocation.count_total, 0);
                    assert_eq!((native.used(), work.used()), (29, 0));
                }
            }
            drop(sibling);
            assert_eq!(native.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
