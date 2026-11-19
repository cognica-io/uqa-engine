//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ir::{RetainedConstantValue, RetainedScalarLiteral},
    plan::retirement::tests::ScalarSource,
};
use std::sync::atomic::{AtomicUsize, Ordering};

type Retirement = ImageDecoder<(), SQLError>;

fn retained(
    depth: usize,
    width: usize,
    native: &MemoryBudget,
    control: &ProductionControl<'_>,
) -> (Arc<Produced<ResolvedWindows>>, Arc<AtomicUsize>) {
    let minimum = Arc::new(AtomicUsize::new(0));
    let retired = Arc::new(AtomicUsize::new(0));
    let mut source = fixture();
    for window in &mut source.windows {
        window.inherited = None;
        window.spec.order_by.clear();
        window.spec.frame = None;
        window.spec.partition_by = (0..width)
            .map(|_| {
                let value = RetainedConstantValue::new(
                    Arc::new(ScalarSource {
                        native: native.clone(),
                        minimum: minimum.clone(),
                        retired: retired.clone(),
                    }),
                    0,
                );
                let mut expression = ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
                    value, None, None, None,
                ));
                for _ in 0..depth {
                    expression = ScalarExpr::Not(Box::new(expression));
                }
                expression
            })
            .collect();
    }
    let resolved = resolved_window_definitions_owned(&source, control).unwrap();
    let resolved = control
        .finish(resolved, control.empty_reservation())
        .unwrap()
        .into_shared(control)
        .unwrap();
    minimum.store(native.used(), Ordering::Relaxed);
    drop(source);
    assert_eq!(retired.load(Ordering::Relaxed), 0);
    (resolved, retired)
}

fn retirement(
    resolved: Arc<Produced<ResolvedWindows>>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Retirement {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        ResolvedWindows::retire_shared_from(resolved, &control, &mut input).await
    })
    .unwrap()
}

fn step(input: &mut Retirement) -> Result<Option<()>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn resolved_window_retirement_bounds_final_shared_payload_release() {
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let (resolved, retired) = retained(0, 128, &native, &control);
    let blocker = native.reserve((8 << 20) - native.used()).unwrap();
    let mut input = retirement(resolved, &native, &work, &token, &token);
    let mut polls = 0;
    loop {
        let before = retired.load(Ordering::Relaxed);
        let result = step(&mut input).unwrap();
        assert!(retired.load(Ordering::Relaxed) - before <= 1);
        polls += 1;
        assert!(polls < 10_000);
        if result.is_some() {
            break;
        }
    }
    assert_eq!(retired.load(Ordering::Relaxed), 384);
    assert_eq!((native.used(), work.used()), (blocker.bytes(), 0));
    drop(blocker);
    assert_eq!((native.used(), work.used()), (0, 0));
}

#[test]
fn resolved_window_retirement_keeps_all_leases_through_every_interruption() {
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let (resolved, retired) = retained(16, 2, &native, &control);
    let mut input = retirement(resolved, &native, &work, &token, &token);
    let mut polls = 0;
    while step(&mut input).unwrap().is_none() {
        polls += 1;
        assert!(polls < 10_000);
    }
    assert_eq!(retired.load(Ordering::Relaxed), 6);
    drop(input);
    let sibling = native.reserve(29).unwrap();
    for prefix in 0..=polls {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&native, &original, &invoking);
            let (resolved, retired) = retained(16, 2, &native, &control);
            let mut input = retirement(resolved, &native, &work, &original, &invoking);
            for _ in 0..prefix {
                assert!(step(&mut input).unwrap().is_none());
            }
            if cancellation != 0 {
                if cancellation == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(step(&mut input).unwrap_err().sqlstate(), Some("57014"));
            }
            let allocation = allocation_counter::measure(|| drop(input));
            assert_eq!(allocation.count_total, 0);
            assert_eq!(retired.load(Ordering::Relaxed), 6);
            assert_eq!((native.used(), work.used()), (29, 0));
        }
    }
    drop(sibling);
    assert_eq!(native.used(), 0);
}

#[test]
fn resolved_window_retirement_suspends_deep_release_and_uses_bounded_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(32 << 20);
            let work = MemoryBudget::new(1 << 20);
            for cancellation in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let control = ProductionControl::new(&native, &original, &invoking);
                let (resolved, retired) = retained(8192, 1, &native, &control);
                let mut input = retirement(resolved, &native, &work, &original, &invoking);
                for _ in 0..64 {
                    assert!(step(&mut input).unwrap().is_none());
                }
                assert_eq!(retired.load(Ordering::Relaxed), 0);
                if cancellation == 0 {
                    input.read(&mut std::io::empty()).unwrap();
                } else {
                    if cancellation == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert_eq!(step(&mut input).unwrap_err().sqlstate(), Some("57014"));
                    let allocation = allocation_counter::measure(|| drop(input));
                    assert_eq!(allocation.count_total, 0);
                }
                assert_eq!(retired.load(Ordering::Relaxed), 3);
                assert_eq!((native.used(), work.used()), (0, 0));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
