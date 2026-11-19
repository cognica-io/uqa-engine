//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::AtomicBool;

struct Key {
    value: usize,
    comparisons: Arc<AtomicBool>,
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}
impl Eq for Key {}
impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        assert!(self.comparisons.load(AtomicOrdering::Relaxed));
        self.value.cmp(&other.value)
    }
}

#[test]
fn prepared_publication_needs_no_further_comparison_or_allowance() {
    for retained in [false, true] {
        for changed in [63, 128] {
            let budget = MemoryBudget::new(1 << 20);
            let comparisons = Arc::new(AtomicBool::new(true));
            let key = |value| Key {
                value,
                comparisons: comparisons.clone(),
            };
            let mut source = BudgetedSharedMap::new(&budget);
            for value in 0..128 {
                source.try_insert(key(value), value).unwrap();
            }
            let old = retained.then(|| source.snapshot());
            let prepared = source.prepare_insert(key(changed)).unwrap();
            let occupied = budget.reserve(budget.available()).unwrap();
            comparisons.store(false, AtomicOrdering::Relaxed);
            prepared.publish(999);
            comparisons.store(true, AtomicOrdering::Relaxed);
            drop(occupied);
            assert_eq!(source.get(&key(changed)), Some(&999));
            assert_eq!(source.len(), 128 + usize::from(changed == 128));
            assert_eq!(verify(&source.root).0, source.len());
            if let Some(old) = old.as_ref() {
                assert!(old
                    .iter()
                    .map(|(key, value)| (key.value, *value))
                    .eq((0..128).map(|value| (value, value))));
            }
            drop((source, old));
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn abandoned_and_denied_preparation_preserve_the_original_root_and_leases() {
    let budget = MemoryBudget::new(1 << 20);
    let mut source = BudgetedSharedMap::new(&budget);
    for value in 0..128 {
        source.try_insert(value, value).unwrap();
    }
    let old = source.snapshot();
    let used = budget.used();
    for changed in [63, 128] {
        let prepared = source.prepare_insert(changed).unwrap();
        assert!(budget.used() > used);
        drop(prepared);
        assert_eq!(budget.used(), used);
        assert!(Arc::ptr_eq(
            source.root.as_ref().unwrap(),
            old.root.as_ref().unwrap()
        ));
        let occupied = budget.reserve(budget.available()).unwrap();
        assert!(source.prepare_insert(changed).is_err());
        drop(occupied);
        assert_eq!(budget.used(), used);
        assert!(source.iter().eq(old.iter()));
    }
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _prepared = source.prepare_insert(128).unwrap();
        panic!("external operation failed before publication");
    }))
    .is_err());
    assert_eq!(budget.used(), used);
    assert!(source.iter().eq(old.iter()));
    drop((source, old));
    assert_eq!(budget.used(), 0);
}
