//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ir::ownership::Retirement, plan::retirement::Pending, ScalarExpr};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    CancellationToken, Value,
};

fn mixed(
    depth: usize,
    scalar_depth: usize,
    width: usize,
    native: &MemoryBudget,
) -> (RewriteMemory, usize) {
    let token = CancellationToken::new();
    let control = ProductionControl::new(native, &token, &token);
    let mut expression = ScalarExpr::Literal(Value::Int(1));
    for _ in 0..scalar_depth {
        expression = ScalarExpr::Not(Box::new(expression));
    }
    let source = Pending::new(expression);
    let (payload, scalar) = source
        .copy_for_rewrite(&control)
        .unwrap()
        .into_rewrite_parts();
    drop(Retirement::new(payload));
    let mut items = BudgetedVec::new(native);
    items.reserve(width + 1).unwrap();
    for _ in 0..width {
        items.push(None).unwrap();
    }
    items.push(scalar).unwrap();
    let mut fields = BudgetedVec::new(native);
    fields
        .push(RewriteField {
            name: "nested",
            memory: Some(chain(depth, native).unwrap()),
        })
        .unwrap();
    fields
        .push(RewriteField {
            name: "scalars",
            memory: Some(RewriteMemory::Scalars {
                direct: native.reserve(5).unwrap(),
                items,
            }),
        })
        .unwrap();
    (
        RewriteMemory::Fields {
            direct: native.reserve(11).unwrap(),
            fields,
        },
        16 + 8 * depth + scalar_depth * size_of::<ScalarExpr>(),
    )
}

fn input(
    memory: RewriteMemory,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<(), SQLError> {
    let native = native.clone();
    let original = original.clone();
    let folding = Folding::new(memory);
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(&native, &original, work.cancellation);
        folding.retire_from(Some(&control), &mut work).await
    })
    .unwrap()
}

fn advance(reader: &mut ImageDecoder<(), SQLError>) -> Result<Option<()>, SQLError> {
    reader.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(reader: &mut ImageDecoder<(), SQLError>) -> usize {
    for polls in 1..1_000_000 {
        if advance(reader).unwrap().is_some() {
            return polls;
        }
    }
    panic!("constructor retirement stopped progressing")
}

#[test]
fn constructor_record_folding_preserves_exact_payload_leases_between_steps() {
    let native = MemoryBudget::new(8 << 20);
    let sibling = native.reserve(47).unwrap();
    let (memory, expected) = mixed(64, 128, 256, &native);
    let mut folding = Folding::new(memory);
    let mut steps = 0;
    while !folding.is_complete() {
        assert!(native.used() >= expected + sibling.bytes());
        let before = native.used();
        let allocated = allocation_counter::measure(|| {
            folding.advance();
        });
        assert_eq!(allocated.count_total, 0);
        assert!(native.used() <= before);
        steps += 1;
    }
    assert!(steps > 64 + 128 + 256);
    let retained = folding.finish().unwrap();
    assert_eq!(retained.bytes(), expected);
    assert_eq!(native.used(), expected + sibling.bytes());
    drop(retained);
    assert_eq!(native.used(), sibling.bytes());
}

#[test]
fn constructor_record_retirement_releases_every_suspended_and_cancelled_prefix() {
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let sibling = native.reserve(47).unwrap();
    let polls = complete(&mut input(
        mixed(4, 12, 8, &native).0,
        &native,
        &work,
        &token,
        &token,
    ));
    for prefix in 0..polls - 1 {
        for cancel in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let (memory, expected) = mixed(4, 12, 8, &native);
            let mut reader = input(memory, &native, &work, &original, &invoking);
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
                assert!(native.used() >= expected + sibling.bytes());
            }
            if cancel != 0 {
                if cancel == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
            }
            assert_eq!(allocation_counter::measure(|| drop(reader)).count_total, 0);
            assert_eq!(
                (native.used(), work.used()),
                (47, 0),
                "prefix={prefix}, cancellation={cancel}"
            );
        }
    }
}

#[test]
fn constructor_record_retirement_bounds_deep_work_without_new_allowance() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(64 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let (memory, expected) = mixed(8192, 8192, 4096, &native);
            let mut reader = input(memory, &native, &work, &token, &token);
            let exhausted = native.reserve(native.limit() - native.used()).unwrap();
            for _ in 0..64 {
                assert!(advance(&mut reader).unwrap().is_none());
                assert!(native.used() >= expected + exhausted.bytes());
            }
            assert!(complete(&mut reader) > 8192);
            assert_eq!(native.used(), exhausted.bytes());
            assert_eq!(allocation_counter::measure(|| drop(reader)).count_total, 0);
            drop(exhausted);
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}
