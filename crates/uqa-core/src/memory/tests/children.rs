//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn child_limits_never_replace_ancestor_limits_and_failed_grow_rolls_back() {
    let root = MemoryBudget::new(100);
    let left = root.child(80);
    let leaf = left.child(usize::MAX);
    let right = root.child(100);
    let sibling = right.reserve(50).unwrap();
    let mut memory = leaf.reserve(40).unwrap();
    assert_eq!(
        (root.used(), left.used(), leaf.used(), right.used()),
        (90, 40, 40, 50)
    );
    assert!(memory.grow(11).is_err());
    assert_eq!(
        (root.used(), left.used(), leaf.used(), memory.bytes()),
        (90, 40, 40, 40)
    );
    drop(sibling);
    assert!(memory.grow(41).is_err());
    assert_eq!((root.used(), left.used(), leaf.used()), (40, 40, 40));
    memory.grow(40).unwrap();
    let split = memory.split(30);
    memory.absorb(split);
    assert_eq!(
        (root.used(), left.used(), leaf.used(), memory.bytes()),
        (80, 80, 80, 80)
    );
    drop(memory);
    assert_eq!(
        (root.used(), left.used(), leaf.used(), right.used()),
        (0, 0, 0, 0)
    );
}

#[test]
fn child_buffer_growth_and_shared_lifetimes_keep_the_original_owner_charged() {
    let root = MemoryBudget::new(256);
    let child = root.child(128);
    let mut values = BudgetedVec::new(&child);
    values.extend_from_slice(&[1_u8; 60]).unwrap();
    assert!(values.extend_from_slice(&[2_u8; 20]).is_err());
    assert_eq!(values.len(), 60);
    assert_eq!((root.used(), child.used()), (60, 60));
    let (values, lease) = values.into_parts();
    let shared = Budgeted::new(values, lease).into_shared().unwrap();
    let retained = Arc::clone(&shared);
    let used = root.used();
    assert!(used > 60);
    drop(child);
    drop(shared);
    assert_eq!(root.used(), used);
    drop(retained);
    assert_eq!(root.used(), 0);
}

#[test]
fn concurrent_siblings_compete_for_the_same_parent_without_overcommit() {
    let root = MemoryBudget::new(32);
    let ready = std::sync::Barrier::new(9);
    let release = std::sync::Barrier::new(9);
    let successes = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let child = root.child(8);
            let (ready, release, successes) = (&ready, &release, &successes);
            scope.spawn(move || {
                let lease = child.reserve(8).ok();
                if lease.is_some() {
                    successes.fetch_add(1, Ordering::Relaxed);
                }
                ready.wait();
                release.wait();
                drop(lease);
                assert_eq!(child.used(), 0);
            });
        }
        ready.wait();
        assert_eq!(successes.load(Ordering::Relaxed), 4);
        assert_eq!(root.used(), 32);
        release.wait();
    });
    assert_eq!(root.used(), 0);
}

#[test]
fn nested_budget_destruction_and_overflow_leave_no_ancestor_charge() {
    let root = MemoryBudget::new(usize::MAX);
    let mut child = root.clone();
    for _ in 0..10_000 {
        child = child.child(usize::MAX);
    }
    let mut lease = child.reserve(1).unwrap();
    assert!(matches!(
        lease.grow(usize::MAX),
        Err(MemoryError::SizeOverflow)
    ));
    assert_eq!(root.used(), 1);
    drop(child);
    drop(lease);
    assert_eq!(root.used(), 0);
}
