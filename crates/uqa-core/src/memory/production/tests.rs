//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::memory::MemoryError;

#[test]
fn ordinary_and_controlled_result_handoffs_cannot_silently_change_ownership() {
    let budget = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let produced = control.copy_text("owned payload").unwrap();
    let reserved = produced.reserved_bytes();
    let produced = produced.into_uncontrolled().unwrap_err();
    assert_eq!(budget.used(), reserved);
    assert_eq!(&*produced, "owned payload");
    let tracked = produced.into_budgeted().unwrap();
    assert_eq!(tracked.reserved_bytes(), reserved);
    drop(tracked);
    assert_eq!(budget.used(), 0);
    let plain = ProductionControl::uncontrolled()
        .copy_text("ordinary")
        .unwrap();
    let plain = plain.into_budgeted().unwrap_err();
    assert_eq!(plain.into_uncontrolled().unwrap(), "ordinary");
}

#[test]
fn output_vectors_keep_container_and_child_leases_until_handoff() {
    let budget = MemoryBudget::new(1 << 16);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let mut values = ProductionVec::new(control);
    values.reserve(2).unwrap();
    values
        .push_produced(control.copy_text("first").unwrap())
        .unwrap();
    values
        .push_produced(control.copy_text("second").unwrap())
        .unwrap();
    let output = values.finish().unwrap();
    let expected = output.capacity() * size_of::<String>()
        + output.iter().map(String::capacity).sum::<usize>();
    assert_eq!(output.reserved_bytes(), expected);
    assert_eq!(budget.used(), expected);
    drop(output);
    assert_eq!(budget.used(), 0);
    let mut scratch = ProductionVec::new(control);
    scratch.push_copy(12_u64).unwrap();
    assert_eq!(&*scratch, &[12]);
    drop(scratch);
    assert_eq!(budget.used(), 0);
}

#[test]
fn failed_child_production_preserves_existing_owners_and_releases_partial_output() {
    let budget = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let held = control.copy_text("held").unwrap();
    let baseline = budget.used();
    {
        let mut output = ProductionString::new(control);
        output.push_str("small").unwrap();
        assert!(matches!(
            output.push_str(&"large".repeat(4096)),
            Err(ValueRetentionError::Memory(MemoryError::Limit { .. }))
        ));
    }
    assert_eq!(budget.used(), baseline);
    assert_eq!(&*held, "held");
    drop(held);
    assert_eq!(budget.used(), 0);
}

struct CancelBetweenChunks<'a>(&'a CancellationToken);
impl std::fmt::Display for CancelBetweenChunks<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("admitted first chunk")?;
        self.0.cancel();
        formatter.write_str("cancelled next chunk")
    }
}

#[test]
fn formatting_preserves_cancellation_errors_from_either_active_owner() {
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let token = if cancel_original {
            &original
        } else {
            &invoking
        };
        let control = ProductionControl::new(&budget, &original, &invoking);
        let result = control.format(format_args!("prefix {} suffix", CancelBetweenChunks(token)));
        assert!(matches!(result, Err(ValueRetentionError::Cancelled(_))));
        assert!(budget.peak() > 0);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn value_copy_and_formatted_names_share_one_allowance() {
    let budget = MemoryBudget::new(8192);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let value = control.copy_value(&Value::Str("한🙂".repeat(500))).unwrap();
    let retained = budget.used();
    let name = control
        .format(format_args!("numeric({},{})[]", 12, -3))
        .unwrap();
    assert_eq!(&*name, "numeric(12,-3)[]");
    assert_eq!(budget.used(), retained + name.reserved_bytes());
    drop(name);
    assert_eq!(budget.used(), retained);
    drop(value);
    assert_eq!(budget.used(), 0);
}

struct DropBeforeLease(MemoryBudget);
impl Drop for DropBeforeLease {
    fn drop(&mut self) {
        assert_eq!(self.0.used(), 17);
    }
}

#[test]
fn cancelled_final_handoff_drops_the_value_before_releasing_its_lease() {
    let budget = MemoryBudget::new(32);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let memory = control.reserve(17).unwrap();
    cancellation.cancel();
    assert!(matches!(
        control.finish(DropBeforeLease(budget.clone()), memory),
        Err(ValueRetentionError::Cancelled(_))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn produced_copy_slots_can_change_without_changing_container_ownership() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut values = ProductionVec::new(control);
    values.push_copy(1_usize).unwrap();
    values.push_copy(2_usize).unwrap();
    let mut values = values.finish().unwrap();
    let memory = values.reserved_bytes();
    let pointer = values.as_ptr();
    values.as_mut_slice().swap(0, 1);
    assert_eq!(&*values, &[2, 1]);
    assert_eq!(values.as_ptr(), pointer);
    assert_eq!(values.reserved_bytes(), memory);
    assert_eq!(budget.used(), memory);
    drop(values);
    assert_eq!(budget.used(), 0);
}

struct ObserveOwnerDrop {
    budget: MemoryBudget,
    observed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Drop for ObserveOwnerDrop {
    fn drop(&mut self) {
        self.observed
            .store(self.budget.used(), std::sync::atomic::Ordering::Relaxed);
    }
}

#[test]
fn vector_input_owner_is_checked_before_capacity_and_dropped_before_its_lease() {
    for (source_controlled, target_controlled) in [(true, true), (true, false), (false, true)] {
        let source_budget = MemoryBudget::new(32);
        let target_budget = MemoryBudget::new(0);
        let cancellation = CancellationToken::new();
        let source = if source_controlled {
            ProductionControl::new(&source_budget, &cancellation, &cancellation)
        } else {
            ProductionControl::uncontrolled()
        };
        let target = if target_controlled {
            ProductionControl::new(&target_budget, &cancellation, &cancellation)
        } else {
            ProductionControl::uncontrolled()
        };
        let observed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
        let owner = source
            .finish(
                ObserveOwnerDrop {
                    budget: source_budget.clone(),
                    observed: observed.clone(),
                },
                source.reserve(17).unwrap(),
            )
            .unwrap();
        let expected = source_budget.used();
        let mut values = ProductionVec::new(target);
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| values.push_produced(owner)));
        assert!(result.is_err());
        assert_eq!(
            observed.load(std::sync::atomic::Ordering::Relaxed),
            expected
        );
        assert!(values.is_empty());
        assert_eq!(source_budget.used(), 0);
        assert_eq!(target_budget.used(), 0);
        assert_eq!(target_budget.peak(), 0);
    }
}
