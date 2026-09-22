//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical caches retain their original allowance through replacement, failure, and nested readers.

use super::*;
use crate::key_value::index_view::read_view;
use crate::read_control::StorageReadControl;

fn specs() -> [VectorIndexSpec; 2] {
    [
        VectorIndexSpec::HNSW(HNSWIndexParams::default()),
        VectorIndexSpec::IVF(IVFIndexParams {
            nlist: 2,
            nprobe: 2,
            train_threshold: 2,
        }),
    ]
}

#[test]
fn physical_cache_replacement_keeps_prior_snapshots_charged_until_the_last_reader() {
    for spec in specs() {
        let store = store();
        let control = read_view(&*store, |read| Ok(read.control().clone())).unwrap();
        let backend = super::super::KeyValueStorageBackend::new(store.clone());
        let mut index = backend
            .vector_index("docs", "embedding", 2, spec, VectorIndexOpenMode::Create)
            .unwrap();
        index
            .add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
            .unwrap();
        index.initialize().unwrap();
        let old = index.snapshot().unwrap();
        let old_used = control.memory().used();
        assert!(old_used > 0);
        let nested = old
            .snapshot_with_control(&StorageReadControl::with_limit(0))
            .unwrap();
        assert_eq!(control.memory().used(), old_used);
        index.add(2, vec![-1.0, 0.0]).unwrap();
        let current = index.snapshot().unwrap();
        assert_eq!(current.count().unwrap(), 3);
        assert_eq!(old.count().unwrap(), 2);
        assert_eq!(old.index_kind(), spec.access_method());
        drop((index, backend, store));
        assert!(control.memory().used() > old_used);
        drop(current);
        assert_eq!(control.memory().used(), old_used);
        drop(old);
        assert_eq!(control.memory().used(), old_used);
        assert_eq!(
            nested
                .search_knn(&[1.0, 0.0], 1)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            vec![1]
        );
        drop(nested);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn physical_restore_and_rebuild_fail_without_replacing_retained_readers_or_records() {
    for spec in specs() {
        let store = store();
        let control = read_view(&*store, |read| Ok(read.control().clone())).unwrap();
        let backend = super::super::KeyValueStorageBackend::new(store.clone());
        let mut index = backend
            .vector_index("docs", "embedding", 2, spec, VectorIndexOpenMode::Create)
            .unwrap();
        index
            .add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
            .unwrap();
        index.initialize().unwrap();
        let old = index.snapshot().unwrap();
        let used = control.memory().used();
        let before = store.scan_prefix(&[]).unwrap();
        let full = control
            .memory()
            .reserve(control.memory().limit() - used)
            .unwrap();
        assert!(matches!(
            backend.vector_index("docs", "embedding", 2, spec, VectorIndexOpenMode::Restore),
            Err(StorageBackendError::Memory(_))
        ));
        assert!(matches!(
            index.initialize(),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(old.count().unwrap(), 2);
        assert_eq!(control.memory().used(), control.memory().limit());
        drop(full);
        assert_eq!(control.memory().used(), used);
        assert_eq!(store.scan_prefix(&[]).unwrap(), before);
        index.initialize().unwrap();
        assert_eq!(index.count().unwrap(), 2);
        control.cancellation().cancel();
        assert!(matches!(
            index.snapshot(),
            Err(StorageBackendError::Cancelled(_))
        ));
        drop((index, old, backend, store));
        assert_eq!(control.memory().used(), 0);
    }
}
