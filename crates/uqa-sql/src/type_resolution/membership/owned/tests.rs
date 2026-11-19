//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

type Input = ImageDecoder<OwnedScalarExpr, SQLError>;

fn source(negated: bool) -> ScalarExpr {
    ScalarExpr::InList {
        expr: Box::new(ScalarExpr::Row(vec![
            ScalarExpr::Param(1),
            ScalarExpr::ScalarSubquery(3),
        ])),
        list: vec![
            ScalarExpr::Literal(Value::Null),
            ScalarExpr::Literal(Value::Str("한🙂".into())),
            ScalarExpr::ScalarSubquery(7),
        ],
        negated,
    }
}

fn input(
    source: OwnedScalarExpr,
    positions: Vec<usize>,
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
        rewrite_owned_from(source, &positions, &control, &mut input).await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<OwnedScalarExpr>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn membership_input_matches_stored_rewrite_and_retires_every_suspended_prefix() {
    for negated in [false, true] {
        for positions in [vec![], vec![0], vec![0, 2], vec![0, 1, 2]] {
            let native = MemoryBudget::new(1 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let source = source(negated);
            let ScalarExpr::InList { expr, list, .. } = source.clone() else {
                unreachable!()
            };
            let reference = crate::type_resolution::membership::rewrite(
                expr,
                list,
                negated,
                &crate::type_resolution::membership::MembershipShape {
                    array_items: positions.clone(),
                },
                &ProductionControl::uncontrolled(),
            )
            .unwrap();
            let ordinary = rewrite_owned(
                source.copy_for_rewrite(&control).unwrap(),
                &positions,
                &control,
            )
            .unwrap();
            let mut reader = input(
                source.copy_for_rewrite(&control).unwrap(),
                positions.clone(),
                &native,
                &work,
                &token,
                &token,
            );
            let mut polls = 0;
            let output = loop {
                polls += 1;
                assert!(polls < 10000);
                if let Some(value) = advance(&mut reader).unwrap() {
                    break value;
                }
            };
            assert!(polls > 10);
            assert_eq!(*output, *reference);
            assert_eq!(*ordinary, *output);
            assert_eq!(ordinary.reserved_bytes(), output.reserved_bytes());
            drop((reader, output, ordinary));
            let sibling = native.reserve(37).unwrap();
            for prefix in 0..polls {
                for cancellation in 0..3 {
                    let original = CancellationToken::new();
                    let invoking = CancellationToken::new();
                    let control = ProductionControl::new(&native, &original, &invoking);
                    let mut reader = input(
                        source.copy_for_rewrite(&control).unwrap(),
                        positions.clone(),
                        &native,
                        &work,
                        &original,
                        &invoking,
                    );
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
                    assert_eq!((native.used(), work.used()), (37, 0));
                }
            }
            let owned = source.copy_for_rewrite(&control).unwrap();
            let held = native.reserve(native.limit() - native.used()).unwrap();
            let reader = input(owned, positions, &native, &work, &token, &token);
            assert_eq!(
                reader.read(&mut std::io::empty()).unwrap_err().sqlstate(),
                Some("53200")
            );
            assert_eq!(native.used(), held.bytes() + 37);
            drop((held, sibling));
            assert_eq!((native.used(), work.used()), (0, 0));
        }
    }
}

#[test]
fn membership_input_copies_deep_operands_without_recursive_polls_or_cleanup() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(32 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let mut left = ScalarExpr::Param(1);
            for _ in 0..8192 {
                left = ScalarExpr::Not(Box::new(left));
            }
            let source = OwnedScalarExpr::uncontrolled(ScalarExpr::InList {
                expr: Box::new(left),
                list: vec![ScalarExpr::Param(2), ScalarExpr::Param(3)],
                negated: false,
            });
            let mut reader = input(
                source.copy_for_rewrite(&control).unwrap(),
                vec![],
                &native,
                &work,
                &token,
                &token,
            );
            let mut polls = 0;
            let output = loop {
                polls += 1;
                assert!(polls < 200_000);
                if let Some(value) = advance(&mut reader).unwrap() {
                    break value;
                }
            };
            assert!(polls > 8192);
            assert_eq!(output.child_count(), 2);
            drop((reader, output));
            for prefix in [0, 8, 64, 1024, 8192, polls - 2] {
                for cancellation in 0..3 {
                    let original = CancellationToken::new();
                    let invoking = CancellationToken::new();
                    let control = ProductionControl::new(&native, &original, &invoking);
                    let mut reader = input(
                        source.copy_for_rewrite(&control).unwrap(),
                        vec![],
                        &native,
                        &work,
                        &original,
                        &invoking,
                    );
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
                    assert_eq!((native.used(), work.used()), (0, 0));
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
