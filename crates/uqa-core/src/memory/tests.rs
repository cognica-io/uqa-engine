//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn concurrent_owners_share_one_limit_and_release_their_leases() {
    let budget = MemoryBudget::new(64);
    let ready = std::sync::Barrier::new(9);
    let release = std::sync::Barrier::new(9);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let budget = budget.clone();
            let (ready, release) = (&ready, &release);
            scope.spawn(move || {
                let _memory = budget.reserve(8).unwrap();
                ready.wait();
                release.wait();
            });
        }
        ready.wait();
        assert_eq!(budget.used(), 64);
        assert!(matches!(
            budget.reserve(1),
            Err(MemoryError::Limit {
                required: 65,
                limit: 64
            })
        ));
        release.wait();
    });
    assert_eq!(budget.used(), 0);
    assert_eq!(budget.peak(), 64);
}

#[test]
fn vector_growth_accounts_for_both_buffers_and_preserves_failed_contents() {
    let budget = MemoryBudget::new(5);
    let mut values = BudgetedVec::new(&budget);
    values.reserve(2).unwrap();
    values.push(10u8).unwrap();
    values.push(20).unwrap();
    values.push(30).unwrap(); // Exact growth fits; the preferred doubled capacity does not.
    assert_eq!(values.capacity(), 3);
    assert_eq!(budget.used(), 3);
    assert_eq!(budget.peak(), 5);
    assert!(matches!(values.push(40), Err(MemoryError::Limit { .. })));
    assert_eq!(&*values, &[10, 20, 30]);
    assert_eq!(budget.used(), 3);
    values.clear();
    assert_eq!(budget.used(), 3);
    drop(values);
    assert_eq!(budget.used(), 0);
}

#[test]
fn wrapped_deque_retains_capacity_and_preserves_order_during_growth() {
    let budget = MemoryBudget::new(64);
    let mut values = BudgetedDeque::new(&budget);
    values.reserve(3).unwrap();
    for value in 0u8..3 {
        values.push_back(value).unwrap();
    }
    assert_eq!(values.pop_front(), Some(0));
    values.push_back(3).unwrap();
    values.push_back(4).unwrap();
    assert_eq!(
        (0..4).map(|i| values[i]).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    let retained = budget.used();
    while values.pop_front().is_some() {}
    assert_eq!(budget.used(), retained);
    drop(values);
    assert_eq!(budget.used(), 0);
}

#[test]
fn result_transfer_keeps_the_allowance_until_the_value_is_destroyed() {
    struct ChecksDrop(MemoryBudget);
    impl Drop for ChecksDrop {
        fn drop(&mut self) {
            assert_eq!(self.0.used(), 7);
        }
    }
    let budget = MemoryBudget::new(7);
    let mut memory = budget.reserve(4).unwrap();
    memory.absorb(budget.reserve(3).unwrap());
    let result = Budgeted::new(ChecksDrop(budget.clone()), memory);
    let (value, memory) = result.into_parts();
    assert_eq!(memory.bytes(), 7);
    drop(Budgeted::new(value, memory));
    assert_eq!(budget.used(), 0);
}

#[test]
fn overflowing_requests_fail_without_changing_existing_reservations() {
    let budget = MemoryBudget::new(usize::MAX);
    let mut memory = budget.reserve(1).unwrap();
    assert!(matches!(
        memory.grow(usize::MAX),
        Err(MemoryError::SizeOverflow)
    ));
    assert_eq!(memory.bytes(), 1);
    let mut values = BudgetedVec::<u64>::new(&budget);
    assert!(matches!(
        values.reserve(usize::MAX),
        Err(MemoryError::SizeOverflow)
    ));
    assert_eq!(budget.used(), 1);
}

#[test]
fn zero_sized_elements_need_no_buffer_reservation() {
    let budget = MemoryBudget::new(0);
    let mut values = BudgetedVec::new(&budget);
    for _ in 0..10 {
        values.push(()).unwrap();
    }
    assert_eq!(values.len(), 10);
    assert_eq!(budget.peak(), 0);
}

#[test]
fn unicode_string_growth_fails_without_partial_appends() {
    let budget = MemoryBudget::new(10);
    let mut value = BudgetedString::new(&budget);
    value.push('한').unwrap();
    value.push_str("🙂").unwrap();
    assert_eq!(&*value, "한🙂");
    assert_eq!(budget.used(), 7);
    assert_eq!(budget.peak(), 10);
    assert!(matches!(value.push('A'), Err(MemoryError::Limit { .. })));
    assert_eq!(&*value, "한🙂");
    drop(value);
    assert_eq!(budget.used(), 0);
}

#[test]
fn shared_values_keep_their_payload_and_buffer_leases_until_the_last_owner() {
    let budget = MemoryBudget::new(1024);
    let mut value = BudgetedString::new(&budget);
    value.push_str("한🙂").unwrap();
    let (value, memory) = value.into_parts();
    let shared = Budgeted::new(value, memory).into_shared().unwrap();
    let retained = shared.reserved_bytes();
    assert_eq!(retained, 7 + std::mem::size_of::<Budgeted<String>>());
    let other_owner = shared.clone();
    drop(shared);
    assert_eq!(budget.used(), retained);
    assert_eq!(&***other_owner, "한🙂");
    drop(other_owner);
    assert_eq!(budget.used(), 0);
}

#[test]
fn failure_to_reserve_a_shared_payload_releases_the_consumed_value() {
    let budget = MemoryBudget::new(7);
    let mut value = BudgetedString::new(&budget);
    value.push_str("한🙂").unwrap();
    let (value, memory) = value.into_parts();
    assert!(matches!(
        Budgeted::new(value, memory).into_shared(),
        Err(MemoryError::Limit { .. })
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn split_leases_transfer_live_allocations_without_reacquiring_a_full_allowance() {
    let budget = MemoryBudget::new(12);
    let unrelated = budget.reserve(3).unwrap();
    let mut memory = budget.reserve(9).unwrap();
    let mut first = memory.split(5);
    let second = memory.split(4);
    assert_eq!(memory.bytes(), 0);
    assert_eq!(budget.used(), 12);
    assert_eq!(budget.peak(), 12);
    drop(memory);
    assert_eq!(budget.used(), 12);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| first.split(6))).is_err());
    assert_eq!(first.bytes(), 5);
    first.absorb(second);
    assert_eq!(first.bytes(), 9);
    drop(first);
    assert_eq!(budget.used(), 3);
    drop(unrelated);
    assert_eq!(budget.used(), 0);
}
