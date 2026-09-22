//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current physical-index captures preserve search semantics and retained resource ownership.

use super::{HNSWIndexParams, VectorIndex};
use crate::{read_control::StorageReadControl, HNSWIndex, IVFIndex, StorageBackendError};

fn sources() -> Vec<Box<dyn VectorIndex>> {
    let mut sources: Vec<Box<dyn VectorIndex>> = vec![
        Box::new(HNSWIndex::with_params(2, HNSWIndexParams::default()).unwrap()),
        Box::new(IVFIndex::with_params(2, 2, 1, 4)),
    ];
    for source in &mut sources {
        for (id, vectors) in [
            (9, vec![vec![1.0, 0.0], vec![0.0, 1.0]]),
            (1, vec![vec![1.0, 0.0]]),
            (3, vec![vec![0.0, 0.0]]),
            (5, vec![vec![-1.0, 0.0]]),
            (7, Vec::new()),
        ] {
            source.add_many(id, vectors).unwrap();
        }
    }
    sources
}

#[test]
fn physical_snapshots_preserve_search_and_share_the_original_allowance_until_final_drop() {
    for mut source in sources() {
        let control = StorageReadControl::with_limit(1 << 20);
        let snapshot = source.snapshot_with_control(&control).unwrap();
        assert_eq!(snapshot.index_kind(), source.index_kind());
        assert_eq!(snapshot.count().unwrap(), source.count().unwrap());
        assert!(!snapshot.contains_document(7).unwrap());
        for k in [0, 1, 2, 8] {
            assert_eq!(
                snapshot.search_knn(&[1.0, 0.0], k).unwrap(),
                source.search_knn(&[1.0, 0.0], k).unwrap()
            );
        }
        for threshold in [-1.0, 0.0, 0.5, 1.0] {
            assert_eq!(
                snapshot.search_threshold(&[1.0, 0.0], threshold).unwrap(),
                source.search_threshold(&[1.0, 0.0], threshold).unwrap()
            );
        }
        let expected = snapshot.search_knn(&[1.0, 0.0], 8).unwrap();
        let used = control.memory().used();
        assert!(used > 0);
        let other = StorageReadControl::with_limit(0);
        let mut nested = snapshot.snapshot_with_control(&other).unwrap();
        assert_eq!(other.memory().used(), 0);
        assert_eq!(control.memory().used(), used);
        source.add(1, vec![0.0, 1.0]).unwrap();
        source.delete(9).unwrap();
        drop(source);
        drop(snapshot);
        assert_eq!(nested.search_knn(&[1.0, 0.0], 8).unwrap(), expected);
        assert_eq!(control.memory().used(), used);
        let unique = std::sync::Arc::get_mut(&mut nested).unwrap();
        assert!(unique.add(2, vec![1.0, 0.0]).is_err());
        assert!(unique.clear().is_err());
        drop(nested);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn rejected_physical_captures_release_partial_reservations_and_preserve_existing_readers() {
    for source in sources() {
        let control = StorageReadControl::with_limit(1 << 20);
        let snapshot = source.snapshot_with_control(&control).unwrap();
        let used = control.memory().used();
        let expected = snapshot.search_knn(&[1.0, 0.0], 8).unwrap();
        for limit in [0, used - 1] {
            let rejected = StorageReadControl::with_limit(limit);
            assert!(matches!(
                source.snapshot_with_control(&rejected),
                Err(StorageBackendError::Memory(_))
            ));
            assert_eq!(rejected.memory().used(), 0);
            if limit > 0 {
                assert!(rejected.memory().peak() > 0);
            }
        }
        let full = control
            .memory()
            .reserve(control.memory().limit() - used)
            .unwrap();
        assert!(matches!(
            source.snapshot_with_control(&control),
            Err(StorageBackendError::Memory(_))
        ));
        drop(full);
        control.cancellation().cancel();
        assert!(matches!(
            source.snapshot_with_control(&control),
            Err(StorageBackendError::Cancelled(_))
        ));
        control.cancellation().reset();
        assert_eq!(snapshot.search_knn(&[1.0, 0.0], 8).unwrap(), expected);
        assert_eq!(source.search_knn(&[1.0, 0.0], 8).unwrap(), expected);
        assert_eq!(control.memory().used(), used);
        drop(snapshot);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn physical_queries_release_failed_workspace_and_keep_the_captured_control() {
    for source in sources() {
        let control = StorageReadControl::with_limit(1 << 20);
        let snapshot = source.snapshot_with_control(&control).unwrap();
        let other = StorageReadControl::with_limit(1 << 20);
        let nested = snapshot.snapshot_with_control(&other).unwrap();
        let expected = snapshot.search_knn(&[1.0, 0.0], 8).unwrap();
        let used = control.memory().used();
        for remaining in [0, 64] {
            let held = control
                .memory()
                .reserve(control.memory().limit() - used - remaining)
                .unwrap();
            for reader in [&snapshot, &nested] {
                assert!(matches!(
                    reader.search_knn(&[1.0, 0.0], 8),
                    Err(StorageBackendError::Memory(_))
                ));
                assert!(matches!(
                    reader.search_threshold(&[1.0, 0.0], -1.0),
                    Err(StorageBackendError::Memory(_))
                ));
                assert!(matches!(
                    reader.search_knn_with_control(&[1.0, 0.0], 8, &other),
                    Err(StorageBackendError::Memory(_))
                ));
                assert!(matches!(
                    reader.search_threshold_with_control(&[1.0, 0.0], -1.0, &other),
                    Err(StorageBackendError::Memory(_))
                ));
                assert_eq!(reader.count().unwrap(), source.count().unwrap());
                assert!(reader.search_knn(&[1.0, 0.0], 0).unwrap().is_empty());
                assert_eq!(
                    control.memory().used(),
                    control.memory().limit() - remaining
                );
                assert_eq!(other.memory().used(), 0);
            }
            drop(held);
            assert_eq!(snapshot.search_knn(&[1.0, 0.0], 8).unwrap(), expected);
            assert_eq!(control.memory().used(), used);
        }
        control.cancellation().cancel();
        for reader in [&snapshot, &nested] {
            assert!(matches!(
                reader.search_knn_with_control(&[1.0, 0.0], 8, &other),
                Err(StorageBackendError::Cancelled(_))
            ));
            assert!(matches!(
                reader.search_threshold(&[1.0, 0.0], -1.0),
                Err(StorageBackendError::Cancelled(_))
            ));
            assert!(matches!(
                reader.count(),
                Err(StorageBackendError::Cancelled(_))
            ));
            assert!(matches!(
                reader.contains_document(1),
                Err(StorageBackendError::Cancelled(_))
            ));
        }
        control.cancellation().reset();
        assert_eq!(nested.search_knn(&[1.0, 0.0], 8).unwrap(), expected);
        drop((snapshot, nested));
        assert_eq!(control.memory().used(), 0);
    }
}
