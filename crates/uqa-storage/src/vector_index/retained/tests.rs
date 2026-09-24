//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::MemoryVectorIndex;

fn append(
    builder: &mut RetainedVectorIndexBuilder,
    control: &StorageReadControl,
    id: DocId,
    vectors: Vec<Vec<f32>>,
) -> StorageBackendResult<()> {
    let bytes = vectors.capacity() * size_of::<Vec<f32>>()
        + vectors
            .iter()
            .map(|vector| vector.capacity() * size_of::<f32>())
            .sum::<usize>();
    let memory = control.memory().reserve(bytes)?;
    builder.add_document(id, Budgeted::new(vectors, memory))
}

fn scores(index: &dyn VectorIndex, query: &[f32], k: usize) -> Vec<(DocId, f64)> {
    index
        .search_knn(query, k)
        .unwrap()
        .iter()
        .map(|entry| (entry.doc_id, entry.payload.score))
        .collect()
}

#[test]
fn selected_tensors_move_without_copying_and_match_memory_search_after_identity_sort() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut builder = RetainedVectorIndexBuilder::new(2, &control);
    let mut reference = MemoryVectorIndex::new(2);
    let mut first = Vec::with_capacity(4096);
    first.extend_from_slice(&[1.0, 0.0]);
    let address = first.as_ptr();
    for (id, vectors) in [
        (9, vec![first, vec![0.0, 1.0]]),
        (1, vec![vec![1.0, 0.0]]),
        (u64::MAX, vec![vec![0.0, 0.0]]),
        (3, Vec::new()),
    ] {
        reference.add_many(id, vectors.clone()).unwrap();
        append(&mut builder, &control, id, vectors).unwrap();
    }
    let index = builder.finish().unwrap();
    assert_eq!(
        index
            .entries
            .iter()
            .find(|entry| entry.0 == 9 && entry.1 == 0)
            .unwrap()
            .2
            .as_ptr(),
        address
    );
    assert!(control.memory().used() >= 4096 * size_of::<f32>());
    assert_eq!(index.index_kind(), reference.index_kind());
    assert_eq!(index.count().unwrap(), reference.count().unwrap());
    assert!(!index.contains_document(3).unwrap());
    for k in [0, 1, 2, 8] {
        assert_eq!(
            scores(&index, &[1.0, 0.0], k),
            scores(&reference, &[1.0, 0.0], k)
        );
    }
    for threshold in [-1.0, 0.5, 1.0] {
        assert_eq!(
            index
                .search_threshold(&[1.0, 0.0], threshold)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            reference
                .search_threshold(&[1.0, 0.0], threshold)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>()
        );
    }
    assert!(index.search_threshold(&[1.0, 0.0], f32::NAN).is_err());
    drop(index);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn nested_snapshots_share_the_payload_allowance_and_refuse_unbounded_search_workspace() {
    let control = StorageReadControl::with_limit(4096);
    let mut builder = RetainedVectorIndexBuilder::new(2, &control);
    append(&mut builder, &control, 4, vec![vec![1.0, 0.0]]).unwrap();
    let index = builder.finish().unwrap();
    let retained = control.memory().used();
    let snapshot = index.snapshot().unwrap();
    let mut nested = snapshot.snapshot().unwrap();
    assert_eq!(control.memory().used(), retained);
    drop(index);
    drop(snapshot);
    let full = control
        .memory()
        .reserve(control.memory().limit() - retained)
        .unwrap();
    assert!(matches!(
        nested.search_knn(&[1.0, 0.0], 1),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(full);
    assert_eq!(scores(nested.as_ref(), &[1.0, 0.0], 1), [(4, 1.0)]);
    assert_eq!(control.memory().used(), retained);
    control.cancellation().cancel();
    assert!(matches!(
        nested.search_knn(&[1.0, 0.0], 1),
        Err(StorageBackendError::Cancelled(_))
    ));
    control.cancellation().reset();
    let unique = Arc::get_mut(&mut nested).unwrap();
    assert!(unique.add(9, vec![0.0, 1.0]).is_err());
    assert!(unique.delete(4).is_err());
    assert!(unique.clear().is_err());
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn rejected_vector_payloads_and_invalid_values_leave_earlier_documents_intact() {
    let control = StorageReadControl::with_limit(4096);
    let mut builder = RetainedVectorIndexBuilder::new(2, &control);
    append(&mut builder, &control, 1, vec![vec![1.0, 0.0]]).unwrap();
    let used = control.memory().used();
    let mut large = Vec::with_capacity(4096);
    large.extend_from_slice(&[0.0, 1.0]);
    assert!(matches!(
        append(&mut builder, &control, 2, vec![large]),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), used);
    for invalid in [vec![0.0], vec![f32::INFINITY, 0.0]] {
        assert!(append(&mut builder, &control, 2, vec![invalid]).is_err());
        assert_eq!(control.memory().used(), used);
    }
    control.cancellation().cancel();
    assert!(matches!(
        append(&mut builder, &control, 2, vec![vec![0.0, 1.0]]),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), used);
    control.cancellation().reset();
    let index = builder.finish().unwrap();
    assert_eq!(index.count().unwrap(), 1);
    assert_eq!(scores(&index, &[1.0, 0.0], 1), [(1, 1.0)]);
    drop(index);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn duplicate_inputs_and_cancelled_finalization_cannot_publish_partial_indexes() {
    let control = StorageReadControl::with_limit(4096);
    for cancel in [false, true] {
        let mut builder = RetainedVectorIndexBuilder::new(2, &control);
        append(&mut builder, &control, 1, vec![vec![1.0, 0.0]]).unwrap();
        append(&mut builder, &control, 1, vec![vec![0.0, 1.0]]).unwrap();
        if cancel {
            control.cancellation().cancel();
        }
        assert!(builder.finish().is_err());
        assert_eq!(control.memory().used(), 0);
        control.cancellation().reset();
    }
}

#[test]
fn adopted_buffers_transfer_their_original_reservation_without_a_second_payload_charge() {
    let vectors = vec![vec![1.0; 512], vec![0.0; 512]];
    let pointers = vectors.iter().map(Vec::as_ptr).collect::<Vec<_>>();
    let payload = vectors
        .iter()
        .map(|vector| vector.capacity() * size_of::<f32>())
        .sum::<usize>();
    let headers = vectors.capacity() * size_of::<Vec<f32>>();
    let entries = vectors.len() * size_of::<(DocId, u32, Vec<f32>)>();
    let control = StorageReadControl::with_limit(
        payload + headers + entries + size_of::<Budgeted<VectorEntries>>(),
    );
    let mut builder = RetainedVectorIndexBuilder::new(512, &control);
    append(&mut builder, &control, 1, vectors).unwrap();
    assert_eq!(control.memory().used(), payload + entries);
    let index = builder.finish().unwrap();
    assert_eq!(
        index
            .entries
            .iter()
            .map(|entry| entry.2.as_ptr())
            .collect::<Vec<_>>(),
        pointers,
    );
    drop(index);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn foreign_or_incomplete_reservations_cannot_replace_the_original_index_allowance() {
    let control = StorageReadControl::with_limit(4096);
    let other = StorageReadControl::with_limit(4096);
    let mut builder = RetainedVectorIndexBuilder::new(2, &control);
    append(&mut builder, &control, 1, vec![vec![1.0, 0.0]]).unwrap();
    let before = control.memory().used();
    assert!(append(&mut builder, &other, 2, vec![vec![0.0, 1.0]]).is_err());
    assert_eq!(other.memory().used(), 0);
    assert_eq!(control.memory().used(), before);
    assert!(builder
        .add_document(
            2,
            Budgeted::new(vec![vec![0.0, 1.0]], control.memory().empty_reservation()),
        )
        .is_err());
    assert_eq!(control.memory().used(), before);
    let index = builder.finish().unwrap();
    assert_eq!(scores(&index, &[1.0, 0.0], 1), [(1, 1.0)]);
    drop(index);
    assert_eq!(control.memory().used(), 0);
}
