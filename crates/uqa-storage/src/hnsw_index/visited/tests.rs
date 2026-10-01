//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::read_control::StorageReadControl;

#[test]
fn dense_and_sparse_membership_match_across_word_and_identity_boundaries() {
    for first in [0, 1, u64::MAX - 1024] {
        let mut dense = Visited::new(Some(first), Some(first + 1024), 8, None).unwrap();
        assert!(matches!(dense, Visited::Dense { .. }));
        let mut sparse = Visited::new(Some(first), Some(first + 1024), 1, None).unwrap();
        assert!(matches!(sparse, Visited::Sparse(_)));
        for offset in (0..=1024).step_by(3).chain([0, 1, 63, 64, 65, 1024, 1, 64]) {
            assert_eq!(
                dense.insert(first + offset).unwrap(),
                sparse.insert(first + offset).unwrap()
            );
        }
    }
    assert!(matches!(
        Visited::new(Some(0), Some(u64::MAX), 64, None).unwrap(),
        Visited::Sparse(_)
    ));
}

#[test]
fn dense_workspace_retains_its_original_budget_and_checks_cancellation() {
    let control = StorageReadControl::with_limit(128);
    {
        let mut set = Visited::new(Some(1), Some(512), 64, Some(&control)).unwrap();
        assert_eq!(control.memory().used(), 64);
        assert!(set.insert(512).unwrap());
        assert!(!set.insert(512).unwrap());
    }
    assert_eq!(control.memory().used(), 0);
    assert!(Visited::new(Some(1), Some(4096), 64, Some(&control)).is_err());
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert!(Visited::new(Some(1), Some(512), 64, Some(&control)).is_err());
}
