//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::schema::retention::CatalogRetentionError;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn text(length: usize) -> ScalarExpr {
    ScalarExpr::Literal(Value::Str("x".repeat(length)))
}

fn suspend_ancestors<'s, 'c>(
    source: &'s ScalarExpr,
    control: ProductionControl<'c>,
    mutable: bool,
) -> (Stack<'s, 'c>, Frame<'s, 'c>) {
    let mut stack = Stack::new(control);
    let mut current = Frame::new(source, control, mutable).unwrap();
    for _ in 0..8 {
        let child = current.next_child().unwrap().unwrap();
        stack.push(current).unwrap();
        current = Frame::new(child, control, mutable).unwrap();
    }
    (stack, current)
}

fn wrapped(mut expression: ScalarExpr) -> ScalarExpr {
    for _ in 0..8 {
        expression = ScalarExpr::Not(Box::new(expression));
    }
    expression
}

fn cancel_partial(
    source: &ScalarExpr,
    control: ProductionControl<'_>,
    mutable: bool,
    cancel: &CancellationToken,
    boundary: usize,
) -> Result<()> {
    let (_stack, mut frame) = suspend_ancestors(source, control, mutable);
    let first = frame.next_child()?.expect("first row child");
    frame.attach(copy(first, control, mutable)?)?;
    if boundary == 0 {
        cancel.cancel();
        return frame.next_child().map(drop);
    }
    let second = frame.next_child()?.expect("second row child");
    let child = copy(second, control, mutable)?;
    if boundary == 1 {
        cancel.cancel();
        return frame.attach(child);
    }
    frame.attach(child)?;
    assert!(frame.next_child()?.is_none());
    cancel.cancel();
    frame.finish().map(drop)
}

#[test]
fn cancellation_during_scalar_construction_releases_suspended_and_completed_owners() {
    // Cancelled checks consult thread-scoped deferrals. This runtime handle belongs to the test thread, outside scalar construction.
    let _thread = std::thread::current();
    let source = wrapped(ScalarExpr::Row(vec![text(97), text(203)]));
    for mutable in [false, true] {
        for original_cancel in [false, true] {
            for boundary in 0..3 {
                let budget = MemoryBudget::new(16384);
                let stable = CancellationToken::new();
                let retained = ProductionControl::new(&budget, &stable, &stable)
                    .copy_text("independent owner")
                    .unwrap();
                let before = budget.used();
                let measured = allocation_counter::measure(|| {
                    // Token-owned synchronization may allocate lazily during cancellation. Include the complete token lifetime in this measurement.
                    let original = CancellationToken::new();
                    let invoking = CancellationToken::new();
                    let control = ProductionControl::new(&budget, &original, &invoking);
                    let cancel = if original_cancel {
                        &original
                    } else {
                        &invoking
                    };
                    assert!(matches!(
                        cancel_partial(&source, control, mutable, cancel, boundary),
                        Err(CatalogRetentionError::Cancelled(_))
                    ));
                });
                assert!(measured.count_total > 0);
                assert_eq!(measured.bytes_current, 0);
                assert_eq!(budget.used(), before);
                assert_eq!(&*retained, "independent owner");
                drop(retained);
                assert_eq!(budget.used(), 0);
            }
        }
    }
}

#[test]
fn unwinding_partial_case_construction_releases_unpaired_condition_and_pending_result() {
    let source = wrapped(ScalarExpr::Case {
        base: None,
        when: vec![(text(97), text(203))],
        else_branch: None,
    });
    for mutable in [false, true] {
        let budget = MemoryBudget::new(16384);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let retained = control.copy_text("independent owner").unwrap();
        let before = budget.used();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (_stack, mut frame) = suspend_ancestors(&source, control, mutable);
            let condition = frame.next_child().unwrap().expect("CASE condition");
            frame
                .attach(copy(condition, control, mutable).unwrap())
                .unwrap();
            let result = frame.next_child().unwrap().expect("CASE result");
            let _pending = copy(result, control, mutable).unwrap();
            panic!("constructor unwind");
        }));
        assert!(result.is_err());
        assert_eq!(budget.used(), before);
        assert_eq!(&*retained, "independent owner");
        drop(retained);
        assert_eq!(budget.used(), 0);
    }
}
