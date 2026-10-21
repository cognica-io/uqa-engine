//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ReadOnlySnapshot, StorageBackendError};
use uqa_core::DocId;

#[test]
fn controlled_memory_snapshot_does_not_copy_the_complete_vector_corpus() {
    let mut source = MemoryVectorIndex::new(128);
    for id in 1..=8192 {
        source.add(id, vec![id as f32; 128]).unwrap();
    }
    let control = StorageReadControl::with_limit(16 * 1024 * 1024);
    let mut snapshot = None;
    let mut writable = None;
    let allocation = allocation_counter::measure(|| {
        snapshot = Some(source.snapshot_with_control(&control).unwrap());
        writable = Some(source.writable_snapshot().unwrap());
    });
    assert_eq!(snapshot.as_ref().unwrap().count().unwrap(), 8192);
    // Sharing coordinates must not waive their original retention allowance.
    assert!(control.memory().used() >= 8192 * 128 * size_of::<f32>());
    assert!(allocation.bytes_max < 64 * 1024, "{allocation:?}");
    let snapshot = snapshot.unwrap();
    let canonical = snapshot.vector_read_snapshot(&control).unwrap().unwrap();
    let before = canonical.corpus_fingerprint(&control).unwrap();
    source.add(1, vec![0.0; 128]).unwrap();
    source.delete(8192).unwrap();
    assert_eq!(
        canonical
            .read_vector(1, 0, &control)
            .unwrap()
            .unwrap()
            .as_ref(),
        &[1.0; 128]
    );
    assert!(snapshot.contains_document(8192).unwrap());
    let mut writable = writable.unwrap();
    writable.add(1, vec![-1.0; 128]).unwrap();
    assert!(writable.contains_document(8192).unwrap());
    assert_eq!(source.vectors()[&1], [vec![0.0; 128]]);
    assert_eq!(
        canonical
            .read_vector(1, 0, &control)
            .unwrap()
            .unwrap()
            .as_ref(),
        &[1.0; 128]
    );
    assert_eq!(canonical.corpus_fingerprint(&control).unwrap(), before);
    drop(canonical);
    drop(snapshot);
    assert_eq!(control.memory().used(), 0);
}

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
fn failed_shared_memory_capture_preserves_source_and_releases_admission() {
    let mut source = MemoryVectorIndex::new(128);
    for id in 0..8 {
        source.add(id, vec![1.0; 128]).unwrap();
    }
    let control = StorageReadControl::with_limit(2048);
    assert!(matches!(
        source.snapshot_with_control(&control),
        Err(StorageBackendError::Memory(_))
    ));
    // The complete existing footprint is rejected before any partial snapshot allocation.
    assert_eq!(control.memory().peak(), 0);
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

#[test]
fn finite_memory_and_retained_knn_keep_only_selected_document_scores() {
    use uqa_core::memory::Budgeted;
    let control = StorageReadControl::with_limit(64 * 1024 * 1024);
    let mut source = MemoryVectorIndex::new(2);
    let mut builder = crate::vector_index::RetainedVectorIndexBuilder::new(2, &control);
    for id in (1..=32768).rev() {
        source.add(id, vec![1.0, 0.0]).unwrap();
        let vectors = vec![vec![1.0, 0.0]];
        let bytes =
            vectors.capacity() * size_of::<Vec<f32>>() + vectors[0].capacity() * size_of::<f32>();
        builder
            .add_document(
                id,
                Budgeted::new(vectors, control.memory().reserve(bytes).unwrap()),
            )
            .unwrap();
    }
    let retained = builder.finish().unwrap();
    let snapshot = source.snapshot_with_control(&control).unwrap();
    let used = control.memory().used();
    let remaining = control
        .memory()
        .reserve(control.memory().limit() - used - 32 * 1024)
        .unwrap();
    for index in [&source as &dyn VectorIndex, &retained, snapshot.as_ref()] {
        for k in [0, 1, 8] {
            let allocation = allocation_counter::measure(|| {
                let result = index
                    .search_knn_with_control(&[1.0, 0.0], k, &control)
                    .unwrap();
                assert_eq!(result.len(), k);
                for (position, entry) in result.iter().enumerate() {
                    assert_eq!(entry.doc_id, position as u64 + 1);
                    assert_eq!(entry.payload.score.to_bits(), 1.0_f64.to_bits());
                }
            });
            assert!(allocation.bytes_max < 32 * 1024, "k={k}: {allocation:?}");
        }
        assert!(matches!(
            index.search_knn_with_control(&[1.0, 0.0], usize::MAX, &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(index.count().unwrap(), 32768);
    }
    drop(remaining);
    assert_eq!(control.memory().used(), used);
}

#[test]
fn shared_vector_retention_covers_spare_tensor_capacity_before_publication() {
    let mut source = MemoryVectorIndex::new(2);
    let mut vector = Vec::with_capacity(4096);
    vector.extend_from_slice(&[1.0, -0.0]);
    let mut tensor = Vec::with_capacity(128);
    tensor.push(vector);
    let capacity =
        tensor.capacity() * size_of::<Vec<f32>>() + tensor[0].capacity() * size_of::<f32>();
    source.add_many(1, tensor).unwrap();
    let small = StorageReadControl::with_limit(4096);
    assert!(matches!(
        source.snapshot_with_control(&small),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(small.memory().used(), 0);
    let control = StorageReadControl::with_limit(64 * 1024);
    let snapshot = source.snapshot_with_control(&control).unwrap();
    assert!(control.memory().used() >= capacity);
    let canonical = snapshot.vector_read_snapshot(&control).unwrap().unwrap();
    assert_eq!(
        canonical.read_vector(1, 0, &control).unwrap().unwrap()[1].to_bits(),
        (-0.0_f32).to_bits()
    );
    drop(source);
    drop(snapshot);
    assert!(control.memory().used() >= capacity);
    drop(canonical);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn canonical_vector_exports_and_writable_copies_preserve_independent_values() {
    use std::collections::BTreeMap;
    let mut source = MemoryVectorIndex::new(2);
    source
        .add_many(9, vec![vec![1.0, -0.0], vec![0.0, 1.0]])
        .unwrap();
    source.add(1, vec![0.5, 0.5]).unwrap();
    let exported: &BTreeMap<DocId, Vec<Vec<f32>>> = source.vectors();
    assert_eq!(exported.keys().copied().collect::<Vec<_>>(), [1, 9]);
    assert_eq!(exported[&9][0][1].to_bits(), (-0.0_f32).to_bits());
    let snapshot = source.snapshot().unwrap();
    let expected = scores(snapshot.as_ref(), 2);
    let mut cloned = source.clone();
    let mut writable = source.writable_snapshot().unwrap();
    assert!(source.add(9, vec![1.0]).is_err());
    assert_eq!(source.vectors()[&9].len(), 2);
    source.delete(9).unwrap();
    cloned.add(9, vec![0.0, 0.0]).unwrap();
    writable.clear().unwrap();
    assert_eq!(source.vectors().keys().copied().collect::<Vec<_>>(), [1]);
    assert_eq!(cloned.vectors()[&9], [vec![0.0, 0.0]]);
    assert_eq!(scores(snapshot.as_ref(), 2), expected);
    assert_eq!(writable.count().unwrap(), 0);
}

#[test]
fn shared_canonical_identity_matches_owned_tensor_content_and_survives_writes() {
    use uqa_core::memory::Budgeted;
    let control = StorageReadControl::with_limit(1 << 20);
    let mut source = MemoryVectorIndex::new(2);
    let mut builder = crate::vector_index::RetainedVectorIndexBuilder::new(2, &control);
    for (id, vectors) in [
        (9, vec![vec![1.0, -0.0], vec![0.0, 1.0]]),
        (1, vec![vec![0.5, 0.5]]),
    ] {
        source.add_many(id, vectors.clone()).unwrap();
        let bytes = vectors.capacity() * size_of::<Vec<f32>>()
            + vectors
                .iter()
                .map(|vector| vector.capacity() * size_of::<f32>())
                .sum::<usize>();
        builder
            .add_document(
                id,
                Budgeted::new(vectors, control.memory().reserve(bytes).unwrap()),
            )
            .unwrap();
    }
    let retained = builder.finish().unwrap();
    let snapshot = source.snapshot_with_control(&control).unwrap();
    let canonical = snapshot.vector_read_snapshot(&control).unwrap().unwrap();
    let expected = retained
        .vector_read_snapshot(&control)
        .unwrap()
        .unwrap()
        .corpus_fingerprint(&control)
        .unwrap();
    assert_eq!(canonical.corpus_fingerprint(&control).unwrap(), expected);
    let second = source.snapshot_with_control(&control).unwrap();
    assert_eq!(
        second
            .vector_read_snapshot(&control)
            .unwrap()
            .unwrap()
            .corpus_fingerprint(&control)
            .unwrap(),
        expected
    );
    source
        .add_many(9, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    let changed = source.snapshot_with_control(&control).unwrap();
    assert_ne!(
        changed
            .vector_read_snapshot(&control)
            .unwrap()
            .unwrap()
            .corpus_fingerprint(&control)
            .unwrap(),
        expected
    );
    assert_eq!(canonical.corpus_fingerprint(&control).unwrap(), expected);
    assert_eq!(
        canonical.next_document_after(None, &control).unwrap(),
        Some(1)
    );
    assert_eq!(
        canonical.next_document_after(Some(1), &control).unwrap(),
        Some(9)
    );
    assert_eq!(
        canonical
            .next_document_after(Some(u64::MAX), &control)
            .unwrap(),
        None
    );
}
