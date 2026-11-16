//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ir::{ConstantValueSource, RetainedConstantValue, RetainedScalarLiteral};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct Source {
    native: MemoryBudget,
    minimum: Arc<AtomicUsize>,
    retired: Arc<AtomicUsize>,
}

impl ConstantValueSource for Source {
    fn read(&self, _: u64) -> std::result::Result<Value, SQLError> {
        panic!("window replacement must not read evaluated constants")
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        assert!(
            self.native.used() >= self.minimum.load(Ordering::Relaxed),
            "old payloads must retire before their constructor reservations"
        );
        self.retired.fetch_add(1, Ordering::Relaxed);
    }
}

fn source(
    depth: usize,
    width: usize,
    native: &MemoryBudget,
    control: &ProductionControl<'_>,
) -> (OwnedScalarExpr, Arc<AtomicUsize>) {
    let minimum = Arc::new(AtomicUsize::new(0));
    let retired = Arc::new(AtomicUsize::new(0));
    let mut source = fixture();
    let ScalarExpr::WindowCall { spec, .. } = &mut Arc::get_mut(&mut source).unwrap().expression
    else {
        unreachable!()
    };
    spec.partition_by = (0..width)
        .map(|_| {
            let value = RetainedConstantValue::new(
                Arc::new(Source {
                    native: native.clone(),
                    minimum: minimum.clone(),
                    retired: retired.clone(),
                }),
                0,
            );
            let mut expression =
                ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(value, None, None, None));
            for _ in 0..depth {
                expression = ScalarExpr::Not(Box::new(expression));
            }
            expression
        })
        .collect();
    spec.order_by.clear();
    spec.frame = None;
    let owned = source.copy_for_rewrite(control).unwrap();
    minimum.store(native.used(), Ordering::Relaxed);
    drop(source);
    assert_eq!(retired.load(Ordering::Relaxed), 0);
    (owned, retired)
}

#[test]
fn retained_window_payload_retirement_limits_each_work_window() {
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let (owned, retired) = source(0, 128, &native, &control);
    let mut reader = rewrite(owned, definition(0), &native, &work, &token, &token, false);
    let mut polls = 0;
    loop {
        let before = retired.load(Ordering::Relaxed);
        let result = step(&mut reader).unwrap();
        let after = retired.load(Ordering::Relaxed);
        assert!(
            after - before <= 1,
            "one work window retired multiple scalar roots"
        );
        polls += 1;
        assert!(polls < 10_000);
        if let Some(value) = result {
            assert_eq!(after, 128);
            drop(value);
            break;
        }
    }
    drop(reader);
    assert_eq!((native.used(), work.used()), (0, 0));
}

#[test]
fn retained_window_payload_retirement_preserves_owners_at_every_interruption() {
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let (owned, _) = source(32, 2, &native, &control);
    let mut reader = rewrite(owned, definition(0), &native, &work, &token, &token, false);
    let mut polls = 0;
    while step(&mut reader).unwrap().is_none() {
        polls += 1;
        assert!(polls < 10_000);
    }
    drop(reader);
    let sibling = native.reserve(29).unwrap();
    for prefix in 0..=polls {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&native, &original, &invoking);
            let (owned, retired) = source(32, 2, &native, &control);
            let mut reader = rewrite(
                owned,
                definition(0),
                &native,
                &work,
                &original,
                &invoking,
                false,
            );
            for _ in 0..prefix {
                assert!(step(&mut reader).unwrap().is_none());
            }
            if cancellation != 0 {
                if cancellation == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(step(&mut reader).unwrap_err().sqlstate(), Some("57014"));
            }
            let allocation = allocation_counter::measure(|| drop(reader));
            assert_eq!(allocation.count_total, 0);
            assert_eq!(retired.load(Ordering::Relaxed), 2);
            assert_eq!((native.used(), work.used()), (29, 0));
        }
    }
    drop(sibling);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_window_payload_retirement_suspends_before_releasing_deep_leaves() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(16 << 20);
            let work = MemoryBudget::new(1 << 20);
            for cancellation in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let control = ProductionControl::new(&native, &original, &invoking);
                let (owned, retired) = source(8192, 1, &native, &control);
                let mut reader = rewrite(
                    owned,
                    definition(0),
                    &native,
                    &work,
                    &original,
                    &invoking,
                    false,
                );
                for _ in 0..64 {
                    assert!(step(&mut reader).unwrap().is_none());
                }
                assert_eq!(
                    retired.load(Ordering::Relaxed),
                    0,
                    "retirement must suspend before reaching the deep leaf"
                );
                if cancellation == 0 {
                    drop(reader.read(&mut std::io::empty()).unwrap());
                } else {
                    if cancellation == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert_eq!(step(&mut reader).unwrap_err().sqlstate(), Some("57014"));
                    let allocation = allocation_counter::measure(|| drop(reader));
                    assert_eq!(allocation.count_total, 0);
                }
                assert_eq!(retired.load(Ordering::Relaxed), 1);
                assert_eq!((native.used(), work.used()), (0, 0));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn window_specification_copy_admits_its_suspended_workspace() {
    for retained in [false, true] {
        let native = MemoryBudget::new(128 << 10);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&native, &token, &token);
        let (owned, retired) = source(0, 1, &native, &control);
        let blocker = native.reserve((128 << 10) - native.used()).unwrap();
        let definitions = definition(0);
        let error = if retained {
            let mut reader = rewrite(owned, definitions, &native, &work, &token, &token, false);
            let error =
                step(&mut reader).expect_err("deny workspace before the first suspended copy step");
            drop(reader);
            error
        } else {
            SQLError::from(
                owned
                    .replace_window_spec(&definitions[0], &control)
                    .unwrap_err(),
            )
        };
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(retired.load(Ordering::Relaxed), 1);
        assert_eq!((native.used(), work.used()), (blocker.bytes(), 0));
        drop(blocker);
        assert_eq!(native.used(), 0);
    }
}
