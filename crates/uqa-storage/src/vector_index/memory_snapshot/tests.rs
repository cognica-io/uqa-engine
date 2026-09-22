//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ReadOnlySnapshot, StorageBackendError};
use uqa_core::DocId;

fn scores(index: &dyn VectorIndex, k: usize) -> Vec<(DocId, f64)> {
    index
        .search_knn(&[1.0, 0.0], k)
        .unwrap()
        .iter()
        .map(|entry| (entry.doc_id, entry.payload.score))
        .collect()
}

#[test]
fn controlled_memory_snapshots_preserve_tensor_scores_and_retained_lifetimes() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut source = MemoryVectorIndex::new(2);
    for (id, vectors) in [
        (9, vec![vec![1.0, 0.0], vec![0.0, 1.0]]),
        (1, vec![vec![1.0, 0.0]]),
        (3, vec![vec![0.0, 0.0]]),
        (5, vec![vec![-1.0, 0.0]]),
        (7, Vec::new()),
    ] {
        source.add_many(id, vectors).unwrap();
    }
    let snapshot = source.snapshot_with_control(&control).unwrap();
    assert_eq!(snapshot.index_kind(), source.index_kind());
    assert_eq!(snapshot.count().unwrap(), source.count().unwrap());
    assert!(!snapshot.contains_document(7).unwrap());
    for k in [0, 1, 2, 8] {
        assert_eq!(scores(snapshot.as_ref(), k), scores(&source, k));
    }
    for threshold in [-1.0, 0.0, 0.5, 1.0] {
        let selected = |index: &dyn VectorIndex| {
            index
                .search_threshold(&[1.0, 0.0], threshold)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>()
        };
        assert_eq!(selected(snapshot.as_ref()), selected(&source));
    }
    let expected = scores(snapshot.as_ref(), 8);
    let used = control.memory().used();
    assert!(used > 0);
    let nested = snapshot.snapshot_with_control(&control).unwrap();
    assert_eq!(control.memory().used(), used);
    source.add(1, vec![0.0, 1.0]).unwrap();
    source.delete(9).unwrap();
    drop(source);
    drop(snapshot);
    assert_eq!(scores(nested.as_ref(), 8), expected);
    assert_eq!(control.memory().used(), used);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn controlled_snapshots_preserve_tensor_score_order_when_finite_values_overflow() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut source = MemoryVectorIndex::new(2);
    let query = [1.0e30, 0.0];
    let large = vec![1.0e30, 0.0];
    assert!(super::super::cosine_similarity(&query, &large).is_nan());
    source
        .add_many(1, vec![vec![1.0, 0.0], large.clone()])
        .unwrap();
    source.add_many(2, vec![large, vec![1.0, 0.0]]).unwrap();
    source.add(3, vec![0.0, 1.0]).unwrap();
    let snapshot = source.snapshot_with_control(&control).unwrap();
    for k in [1, 8] {
        let bits = |index: &dyn VectorIndex| {
            index
                .search_knn(&query, k)
                .unwrap()
                .iter()
                .map(|entry| (entry.doc_id, entry.payload.score.to_bits()))
                .collect::<Vec<_>>()
        };
        assert_eq!(bits(snapshot.as_ref()), bits(&source));
    }
    for threshold in [0.0, 0.5] {
        let selected = |index: &dyn VectorIndex| {
            index
                .search_threshold(&query, threshold)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>()
        };
        assert_eq!(selected(snapshot.as_ref()), selected(&source));
    }
}

#[test]
fn failed_partial_memory_capture_releases_every_unpublished_buffer() {
    let mut source = MemoryVectorIndex::new(128);
    for id in 0..8 {
        source.add(id, vec![1.0; 128]).unwrap();
    }
    let control = StorageReadControl::with_limit(2048);
    assert!(matches!(
        source.snapshot_with_control(&control),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(control.memory().peak() > 128 * size_of::<f32>());
    assert_eq!(control.memory().used(), 0);
    assert_eq!(source.count().unwrap(), 8);
    assert_eq!(source.search_knn(&[1.0; 128], 1).unwrap().len(), 1);
    control.cancellation().cancel();
    assert!(matches!(
        source.snapshot_with_control(&control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn read_only_adapters_forward_capture_quota_and_keep_previous_readers() {
    let mut source = MemoryVectorIndex::new(2);
    source.add(1, vec![1.0, 0.0]).unwrap();
    let source = ReadOnlySnapshot::new(Arc::new(source) as Arc<dyn VectorIndex>);
    let control = StorageReadControl::with_limit(4096);
    let mut retained = source.snapshot_with_control(&control).unwrap();
    let used = control.memory().used();
    let full = control
        .memory()
        .reserve(control.memory().limit() - used)
        .unwrap();
    assert!(matches!(
        source.snapshot_with_control(&control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(full);
    control.cancellation().cancel();
    assert!(matches!(
        source.snapshot_with_control(&control),
        Err(StorageBackendError::Cancelled(_))
    ));
    control.cancellation().reset();
    assert_eq!(scores(retained.as_ref(), 1), [(1, 1.0)]);
    assert_eq!(control.memory().used(), used);
    let unique = Arc::get_mut(&mut retained).unwrap();
    assert!(unique.add(2, vec![0.0, 1.0]).is_err());
    assert!(unique.clear().is_err());
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}
