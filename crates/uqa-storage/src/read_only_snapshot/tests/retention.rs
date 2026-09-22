//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::{Budgeted, MemoryBudget};

#[test]
fn shared_capture_releases_the_value_before_its_allowance_on_failure_and_last_drop() {
    struct Observed(MemoryBudget, Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Observed {
        fn drop(&mut self) {
            assert!(self.0.used() >= 64);
            self.1.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    for limit in [64, 64 + size_of::<Observed>(), 4096] {
        let memory = MemoryBudget::new(limit);
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let value = Budgeted::new(
            Observed(memory.clone(), dropped.clone()),
            memory.reserve(64).unwrap(),
        );
        let retained = ReadOnlySnapshot::from_budgeted(value);
        if limit == 4096 {
            let retained = retained.unwrap();
            let nested = retained.clone();
            drop(retained);
            assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));
            assert!(memory.used() > 64);
            drop(nested);
        } else {
            assert!(matches!(
                retained,
                Err(crate::StorageBackendError::Memory(_))
            ));
        }
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn budgeted_vector_snapshots_share_the_capture_without_copying_into_another_allowance() {
    let control = StorageReadControl::with_limit(4096);
    let mut index = MemoryVectorIndex::new(2);
    index.add(1, vec![1.0, 0.0]).unwrap();
    let retained = ReadOnlySnapshot::from_budgeted(Budgeted::new(
        index,
        control.memory().reserve(1024).unwrap(),
    ))
    .unwrap();
    let used = control.memory().used();
    let other = StorageReadControl::with_limit(0);
    let mut nested = retained.snapshot_with_control(&other).unwrap();
    assert_eq!(control.memory().used(), used);
    assert_eq!(other.memory().used(), 0);
    drop(retained);
    assert_eq!(nested.count().unwrap(), 1);
    assert!(Arc::get_mut(&mut nested).unwrap().clear().is_err());
    let cancelled = StorageReadControl::with_limit(0);
    cancelled.cancellation().cancel();
    assert!(matches!(
        nested.snapshot_with_control(&cancelled),
        Err(crate::StorageBackendError::Cancelled(_))
    ));
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}
