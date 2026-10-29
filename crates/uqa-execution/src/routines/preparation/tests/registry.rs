//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, MemoryReservation},
    CancellationToken,
};

#[test]
fn preparation_headers_remain_admitted_until_the_last_strong_or_weak_owner() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let registry = PLpgSQLPreparationRegistry::default();
    let owner = registry.register(&control).unwrap();
    let header = size_of::<PLpgSQLPreparations>() + size_of::<MemoryReservation>();
    let node = OwnedMap::<usize, PreparationObserver>::entry_bytes();
    assert_eq!(budget.used(), header + node);
    let mut shared = None;
    let allocation = allocation_counter::measure(|| shared = Some(owner.clone()));
    assert_eq!(allocation.bytes_total, 0);
    drop(owner);
    registry.invalidate_execution_plans();
    assert_eq!(budget.used(), header + node);
    drop(shared);
    // The registry's weak handle still owns the Arc allocation, not its fragments.
    assert_eq!(budget.used(), header + node);
    registry.invalidate_execution_plans();
    assert_eq!(budget.used(), 0);

    let owner = registry.register(&control).unwrap();
    drop(registry);
    assert_eq!(budget.used(), header);
    drop(owner);
    assert_eq!(budget.used(), 0);
}

#[test]
fn failed_registration_releases_partial_headers_and_preserves_live_observers() {
    for failure in 0..3 {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let registry = PLpgSQLPreparationRegistry::default();
        let owner = registry.register(&control).unwrap();
        let before = budget.used();
        let header = size_of::<PLpgSQLPreparations>() + size_of::<MemoryReservation>();
        let occupied = (failure == 0).then(|| budget.reserve(budget.available() - header).unwrap());
        if failure == 1 {
            original.cancel();
        }
        if failure == 2 {
            invoking.cancel();
        }
        let error = match registry.register(&control) {
            Ok(_) => panic!("registration must be rejected"),
            Err(error) => error,
        };
        assert_eq!(
            error.sqlstate(),
            Some(if failure == 0 { "53200" } else { "57014" })
        );
        assert_eq!(registry.owners.lock().len(), 1);
        assert_eq!(
            budget.used(),
            before + occupied.as_ref().map_or(0, MemoryReservation::bytes)
        );
        drop(occupied);
        drop(owner);
        registry.invalidate_execution_plans();
        assert_eq!(budget.used(), 0);
    }
}
