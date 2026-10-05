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

fn check_reader_controls<T: VectorIndex + crate::vector_index::VectorRead + 'static>(
    source: Budgeted<T>,
    original: &StorageReadControl,
) {
    let value = ReadOnlySnapshot::from_budgeted(source)
        .unwrap()
        .with_canonical_vectors(Some(original))
        .unwrap();
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
    check_canonical_reader_controls(
        old.snapshot.as_ref(),
        current.snapshot.as_ref(),
        original,
        &independent,
    );
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
    assert!(limited.snapshot.contains_document(1).unwrap());
    assert!(!limited.snapshot.contains_document(3).unwrap());
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

fn check_canonical_reader_controls(
    old: &dyn VectorIndex,
    current: &dyn VectorIndex,
    original: &StorageReadControl,
    independent: &StorageReadControl,
) {
    let old_values = old.vector_read_snapshot(original).unwrap().unwrap();
    let current_values = current.vector_read_snapshot(independent).unwrap().unwrap();
    assert_eq!(
        old_values.corpus_fingerprint(original).unwrap(),
        current_values.corpus_fingerprint(independent).unwrap()
    );
    original.cancellation().cancel();
    assert!(old_values.read_vector(1, 0, independent).is_err());
    assert_eq!(
        &*current_values
            .read_vector(1, 0, independent)
            .unwrap()
            .unwrap(),
        &[1.0, 0.0]
    );
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

#[test]
fn bounded_ivf_cache_does_not_inherit_another_readers_cancellation() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut builder = crate::ivf_index::IVFCanonicalBuilder::new(
        2,
        IVFIndexParams {
            nlist: 2,
            nprobe: 2,
            train_threshold: 2,
        },
        &control,
    )
    .unwrap();
    builder.vector(1, 0, &[1.0, 0.0]).unwrap();
    builder.vector(2, 0, &[0.0, 1.0]).unwrap();
    let index = crate::ivf_index::IVFReadIndex::new(builder.finish().unwrap()).unwrap();
    check_reader_controls(index, &control);
}
