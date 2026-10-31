//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionVec},
    CancellationToken,
};
use uqa_sql::ast::{LockStrength, LockWait};

fn locks(name: &str, control: &ProductionControl<'_>) -> Produced<Vec<ResolvedRowLock>> {
    let (qualifier, first) = control.copy_text(name).unwrap().into_parts();
    let (storage_name, second) = control.copy_text("public.v").unwrap().into_parts();
    let (display_name, third) = control.copy_text(name).unwrap().into_parts();
    let mut output = ProductionVec::new(*control);
    output
        .push_produced(
            control
                .finish(
                    ResolvedRowLock {
                        qualifier,
                        storage_name,
                        display_name,
                        strength: LockStrength::ForUpdate,
                        wait: LockWait::NoWait,
                        identity_source: true,
                    },
                    control.combine(control.combine(first, second), third),
                )
                .unwrap(),
        )
        .unwrap();
    output.finish().unwrap()
}

#[test]
fn source_lock_scope_keeps_selected_and_child_readers_until_last_release() {
    let budget = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let wide = "x".repeat(4 << 20);
    let mut scope = CteScope::<()>::new();
    let targets = locks(&wide, &control);
    let payload = targets.reserved_bytes();
    assert!(payload >= 8 << 20);
    let mut active = scope
        .enter_source_row_locks_with_control(targets, &control)
        .unwrap();
    let held = budget.used();
    assert!(held > payload);
    let child = active.clone();
    let mut selected = None;
    let allocation = allocation_counter::measure(|| {
        selected = active.source_row_lock_for_view(&wide, "public.v");
    });
    assert_eq!(allocation.bytes_total, 0);
    let selected = selected.unwrap();
    {
        let empty = control
            .finish(Vec::new(), control.empty_reservation())
            .unwrap();
        let hidden = active
            .enter_source_row_locks_with_control(empty, &control)
            .unwrap();
        assert!(hidden.source_row_lock_for_view(&wide, "public.v").is_none());
    }
    assert!(active.source_row_lock_for_view(&wide, "public.v").is_some());
    drop(active);
    assert!(scope.source_row_lock_for_view(&wide, "public.v").is_none());
    drop((scope, child));
    assert_eq!(budget.used(), held);
    assert_eq!(selected.qualifier, wide);
    assert_eq!(selected.wait, LockWait::NoWait);
    drop(selected);
    assert_eq!(budget.used(), 0);
}

#[test]
fn source_lock_scope_rejects_publication_without_changing_the_enclosing_binding() {
    let budget = MemoryBudget::new(1 << 20);
    let query = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(&budget, &query, &session);
    let mut scope = CteScope::<()>::new();
    let mut active = scope
        .enter_source_row_locks_with_control(locks("parent", &control), &control)
        .unwrap();
    let baseline = budget.used();
    let replacement = locks("child", &control);
    let occupied = budget.reserve(budget.available()).unwrap();
    assert_eq!(
        active
            .enter_source_row_locks_with_control(replacement, &control)
            .err()
            .unwrap()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), baseline + occupied.bytes());
    drop(occupied);
    for token in [&query, &session] {
        let replacement = locks("child", &control);
        token.cancel();
        assert_eq!(
            active
                .enter_source_row_locks_with_control(replacement, &control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), baseline);
        token.reset();
        assert!(active
            .source_row_lock_for_view("parent", "public.v")
            .is_some());
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _child = active
            .enter_source_row_locks_with_control(locks("child", &control), &control)
            .unwrap();
        panic!("consumer unwind");
    }));
    assert!(result.is_err());
    assert_eq!(budget.used(), baseline);
    assert!(active
        .source_row_lock_for_view("parent", "public.v")
        .is_some());
    drop(active);
    assert_eq!(budget.used(), 0);
}
