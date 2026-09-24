//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::IndexState;
use crate::{
    read_control::StorageReadControl,
    vector_index::{HNSWIndexParams, IVFIndexParams},
    HNSWIndex, IVFIndex, ReadOnlySnapshot, StorageBackendError, VectorIndex,
};
use std::sync::Arc;
use uqa_core::{
    memory::{Budgeted, MemoryBudget},
    CancellationToken,
};

fn check_reader_controls<T: VectorIndex + 'static>(
    source: Budgeted<T>,
    original: &StorageReadControl,
) {
    let value = ReadOnlySnapshot::from_budgeted(source).unwrap();
    let mut cached = IndexState {
        snapshot: value.snapshot().unwrap(),
        value,
        revision: Some(7),
        definition_candidate: false,
        control: None,
    };
    let direct = cached
        .value
        .snapshot_with_control(&StorageReadControl::with_limit(0))
        .unwrap();
    assert!(matches!(
        direct.search_knn(&[1.0, 0.0], 1),
        Err(StorageBackendError::Memory(_))
    ));
    drop(direct);
    let old = cached.for_read(original).unwrap();
    let same = cached.for_read(&original.clone()).unwrap();
    assert!(Arc::ptr_eq(&old.snapshot, &same.snapshot));
    let new_signal = CancellationToken::new();
    let independent = StorageReadControl::new(original.memory(), &new_signal);
    let current = cached.for_read(&independent).unwrap();
    assert!(!Arc::ptr_eq(&old.snapshot, &current.snapshot));
    assert!(std::ptr::eq(
        &raw const *old.value,
        &raw const *current.value
    ));
    original.cancellation().cancel();
    assert!(matches!(
        old.snapshot.search_knn(&[1.0, 0.0], 1),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        cached.for_read(original),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(Arc::ptr_eq(
        &current.snapshot,
        &cached.for_read(&independent).unwrap().snapshot
    ));
    assert_eq!(
        current
            .snapshot
            .search_knn(&[1.0, 0.0], 1)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        [1]
    );
    let zero = StorageReadControl::new(&MemoryBudget::new(0), &new_signal);
    let limited = cached.for_read(&zero).unwrap();
    assert!(!Arc::ptr_eq(&current.snapshot, &limited.snapshot));
    assert!(std::ptr::eq(
        &raw const *current.value,
        &raw const *limited.value
    ));
    assert!(matches!(
        limited.snapshot.search_knn(&[1.0, 0.0], 1),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(limited.snapshot.count().unwrap(), 2);
    assert_eq!(zero.memory().used(), 0);
    assert_eq!(
        current
            .snapshot
            .search_knn(&[1.0, 0.0], 1)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        [1]
    );
    drop((cached, old, same, current, limited));
    assert_eq!(original.memory().used(), 0);
}

#[test]
fn unchanged_graphs_reuse_readers_only_for_the_same_allowance_and_cancellation_signal() {
    let vectors = [(1, 0, vec![1.0, 0.0]), (2, 0, vec![0.0, 1.0])];
    let control = StorageReadControl::with_limit(1 << 20);
    let hnsw =
        HNSWIndex::from_canonical_controlled(2, HNSWIndexParams::default(), &vectors, &control)
            .unwrap();
    check_reader_controls(hnsw, &control);
    let control = StorageReadControl::with_limit(1 << 20);
    let ivf = IVFIndex::from_canonical_controlled(
        2,
        IVFIndexParams {
            nlist: 2,
            nprobe: 2,
            train_threshold: 2,
        },
        &vectors,
        &control,
    )
    .unwrap();
    check_reader_controls(ivf, &control);
}
