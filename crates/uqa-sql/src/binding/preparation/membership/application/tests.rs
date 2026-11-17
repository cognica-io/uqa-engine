//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::binding::preparation::{
    ownership::{AnalysisType, AnalysisVec},
    parameters::ConvertedInput,
};
use crate::{ast::BinaryOp, ColumnType};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken, Value,
};

type Input = ImageDecoder<OwnedScalarExpr, SQLError>;
type Fixture = (MembershipAnalysis, OwnedScalarExpr);

fn scalar(value: i64, control: &ProductionControl<'_>) -> OwnedScalarExpr {
    ScalarExpr::Literal(Value::Int(value))
        .copy_for_rewrite(control)
        .unwrap()
}

fn fixture(negated: bool, control: &ProductionControl<'_>) -> Fixture {
    let source = ScalarExpr::InList {
        expr: Box::new(ScalarExpr::Param(1)),
        list: vec![
            ScalarExpr::Literal(Value::Int(10)),
            ScalarExpr::Column("row_value".into()),
            ScalarExpr::Literal(Value::Int(20)),
        ],
        negated,
    }
    .copy_for_rewrite(control)
    .unwrap();
    let mut analysis = MembershipAnalysis {
        array_items: AnalysisVec::new(control),
        array_coercions: AnalysisVec::new(control),
        left_constants: AnalysisVec::new(control),
    };
    for index in [0, 2] {
        analysis.array_items.push(index, control).unwrap();
        analysis
            .array_coercions
            .push(
                (index, AnalysisType::ordinary(ColumnType::BigInteger)),
                control,
            )
            .unwrap();
    }
    for value in [30, 40] {
        analysis
            .left_constants
            .push(
                Some(ConvertedInput {
                    expression: scalar(value, control),
                    session_dependent: false,
                }),
                control,
            )
            .unwrap();
    }
    (analysis, source)
}

struct Copies {
    count: Arc<AtomicUsize>,
    fail: bool,
}

impl<W: ScalarInputWork + Send> Occurrences<W> for Copies
where
    W::Error: Into<SQLError>,
{
    async fn copy(
        &mut self,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<OwnedScalarExpr, SQLError> {
        checkpoint(control, work).await.map_err(Into::into)?;
        self.count.fetch_add(1, Ordering::Relaxed);
        if self.fail {
            return Err(SQLError::Internal("selected occurrence failed".into()));
        }
        Ok(expression)
    }
}

fn input(
    (analysis, expression): Fixture,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    mut copies: Copies,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        analysis
            .apply_from(expression, &control, &mut input, &mut copies)
            .await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<OwnedScalarExpr>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> (OwnedScalarExpr, usize) {
    for polls in 1..250_000 {
        let mut next = None;
        let allocation = allocation_counter::measure(|| next = Some(advance(input)));
        assert!(i128::from(allocation.count_total) - i128::from(allocation.count_current) < 64);
        if let Some(output) = next.unwrap().unwrap() {
            return (output, polls);
        }
    }
    panic!("membership input stopped progressing")
}

fn copies() -> Copies {
    Copies {
        count: Arc::new(AtomicUsize::new(0)),
        fail: false,
    }
}

fn observe(output: &ScalarExpr, negated: bool) {
    let parts = match output {
        ScalarExpr::And(parts) if negated => parts,
        ScalarExpr::Or(parts) if !negated => parts,
        _ => panic!("ordered membership comparisons"),
    };
    assert_eq!(parts.len(), 2);
    let ScalarExpr::Func { args, name, .. } = &parts[0] else {
        panic!()
    };
    assert_eq!(name, if negated { "__all_op" } else { "__any_op" });
    assert_eq!(args[0], ScalarExpr::Literal(Value::Int(30)));
    let ScalarExpr::Array(items) = &args[1] else {
        panic!()
    };
    assert_eq!(items.len(), 2);
    for (item, value) in items.iter().zip([10, 20]) {
        let ScalarExpr::Cast { expr, ty, implicit } = item else {
            panic!()
        };
        assert_eq!(**expr, ScalarExpr::Literal(Value::Int(value)));
        assert_eq!(ty, "pg_catalog.\"int8\"");
        assert!(*implicit);
    }
    let ScalarExpr::Binary { op, lhs, rhs } = &parts[1] else {
        panic!()
    };
    assert_eq!(
        *op,
        if negated {
            BinaryOp::NotEqual
        } else {
            BinaryOp::Equal
        }
    );
    assert_eq!(**lhs, ScalarExpr::Literal(Value::Int(40)));
    assert_eq!(**rhs, ScalarExpr::Column("row_value".into()));
}

#[test]
fn retained_membership_application_shares_coercion_constant_and_occurrence_order() {
    for negated in [false, true] {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let (analysis, expression) = fixture(negated, &control);
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let ordinary = analysis
            .apply_with(expression, &control, &mut |left| {
                calls.set(calls.get() + 1);
                assert_eq!(*left, ScalarExpr::Literal(Value::Int(30)));
                Ok(left)
            })
            .unwrap();
        assert_eq!(calls.get(), 1);
        let copies = copies();
        let observed = copies.count.clone();
        let (output, polls) = complete(&mut input(
            fixture(negated, &control),
            &native,
            &work,
            &token,
            &token,
            copies,
        ));
        observe(&output, negated);
        assert_eq!(*ordinary, *output);
        assert_eq!(ordinary.reserved_bytes(), output.reserved_bytes());
        assert_eq!(observed.load(Ordering::Relaxed), 1);
        assert!(polls > 50);
        drop((ordinary, output));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_membership_application_retires_each_interrupted_or_unadmitted_prefix() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let (output, polls) = complete(&mut input(
        fixture(false, &control),
        &native,
        &work,
        &token,
        &token,
        copies(),
    ));
    drop(output);
    let sibling = native.reserve(37).unwrap();
    let mut denied = 0;
    for prefix in 0..polls {
        for action in 0..4 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&native, &original, &invoking);
            let mut reader = input(
                fixture(false, &control),
                &native,
                &work,
                &original,
                &invoking,
                copies(),
            );
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            let held =
                (action == 3).then(|| native.reserve(native.limit() - native.used()).unwrap());
            if action == 1 || action == 2 {
                if action == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
            } else if action == 3 {
                loop {
                    match advance(&mut reader) {
                        Ok(None) => {}
                        Ok(Some(output)) => {
                            observe(&output, false);
                            break;
                        }
                        Err(error) => {
                            assert_eq!(error.sqlstate(), Some("53200"));
                            denied += 1;
                            break;
                        }
                    }
                }
            }
            assert_eq!(allocation_counter::measure(|| drop(reader)).count_total, 0);
            assert_eq!(
                native.used(),
                37 + held
                    .as_ref()
                    .map_or(0, uqa_core::memory::MemoryReservation::bytes)
            );
            assert_eq!(work.used(), 0);
            drop(held);
        }
    }
    assert!(denied > polls / 2);
    drop(sibling);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_membership_application_preserves_shape_count_and_occurrence_errors() {
    for suspended in [false, true] {
        for failure in 0..3 {
            let native = MemoryBudget::new(1 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let (mut analysis, mut expression) = fixture(false, &control);
            if failure == 0 {
                expression = scalar(1, &control);
            }
            if failure == 1 {
                analysis.left_constants.push(None, &control).unwrap();
            }
            let mut copies = copies();
            copies.fail = true;
            let calls = copies.count.clone();
            let error = if suspended {
                input(
                    (analysis, expression),
                    &native,
                    &work,
                    &token,
                    &token,
                    copies,
                )
                .read(&mut std::io::empty())
                .unwrap_err()
            } else {
                analysis
                    .apply_with(expression, &control, &mut |left| {
                        crate::catalog::work::inline(copies.copy(left, &control, &mut ()))
                    })
                    .unwrap_err()
            };
            let expected = match failure {
                0 => "retained membership no longer matches its syntax",
                1 => "analyzed membership comparison count changed",
                2 => "selected occurrence failed",
                _ => unreachable!(),
            };
            assert!(matches!(error, SQLError::Internal(ref message) if message == expected));
            assert_eq!(calls.load(Ordering::Relaxed), usize::from(failure == 2));
            assert_eq!((native.used(), work.used()), (0, 0));
        }
    }
}

#[test]
fn retained_membership_application_bounds_wide_coercions_and_deep_replacement_retirement() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for deep in [false, true] {
                let native = MemoryBudget::new(64 << 20);
                let work = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let control = ProductionControl::new(&native, &token, &token);
                let count = if deep { 2 } else { 4096 };
                let mut left = ScalarExpr::Param(1);
                let mut target = ColumnType::Integer;
                if deep {
                    for _ in 0..8192 {
                        left = ScalarExpr::Not(Box::new(left));
                        target = ColumnType::Array(Box::new(target));
                    }
                }
                let target = crate::plan::retirement::Pending::new(target);
                let source = OwnedScalarExpr::uncontrolled(ScalarExpr::InList {
                    expr: Box::new(left),
                    list: (0..count)
                        .map(|index| ScalarExpr::Literal(Value::Int(index as i64)))
                        .collect(),
                    negated: false,
                });
                let expression = source.copy_for_rewrite(&control).unwrap();
                drop(source);
                let mut analysis = MembershipAnalysis {
                    array_items: AnalysisVec::new(&control),
                    array_coercions: AnalysisVec::new(&control),
                    left_constants: AnalysisVec::new(&control),
                };
                for index in 0..count {
                    analysis.array_items.push(index, &control).unwrap();
                    analysis
                        .array_coercions
                        .push(
                            (
                                index,
                                AnalysisType::from_produced(
                                    target.clone_with_control(&control).unwrap(),
                                    &control,
                                )
                                .unwrap(),
                            ),
                            &control,
                        )
                        .unwrap();
                }
                drop(target);
                analysis
                    .left_constants
                    .push(
                        Some(ConvertedInput {
                            expression: scalar(99, &control),
                            session_dependent: false,
                        }),
                        &control,
                    )
                    .unwrap();
                let (output, polls) = complete(&mut input(
                    (analysis, expression),
                    &native,
                    &work,
                    &token,
                    &token,
                    copies(),
                ));
                assert!(polls > if deep { 8192 } else { 4096 });
                let ScalarExpr::Func { args, .. } = &*output else {
                    panic!()
                };
                assert_eq!(args[0], ScalarExpr::Literal(Value::Int(99)));
                let ScalarExpr::Array(items) = &args[1] else {
                    panic!()
                };
                assert_eq!(items.len(), count);
                let ScalarExpr::Cast { ty, .. } = &items[0] else {
                    panic!()
                };
                assert_eq!(ty.matches("[]").count(), if deep { 8192 } else { 0 });
                assert_eq!(allocation_counter::measure(|| drop(output)).count_total, 0);
                assert_eq!((native.used(), work.used()), (0, 0));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
