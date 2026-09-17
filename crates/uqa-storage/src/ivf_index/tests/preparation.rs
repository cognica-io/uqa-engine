//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated IVF candidates preserve canonical mutation semantics and caller resource limits.

use super::{IVFIndex, IVFMutation, IVFState};
use crate::{read_control::StorageReadControl, StorageBackendError, VectorIndex};
use uqa_core::memory::MemoryError;

fn trained() -> IVFIndex {
    let mut index = IVFIndex::with_params(3, 2, 2, 2);
    for doc in 1..=10 {
        index
            .add(doc, vec![doc as f32, 11.0 - doc as f32, 0.5])
            .unwrap();
    }
    index.train().unwrap();
    index
}

#[test]
fn candidates_preserve_centroids_counters_and_the_source_generation() {
    let source = trained();
    let before = source.metadata_snapshot();
    let replacement = [vec![0.0, 0.0, 1.0], vec![0.0, 1.0, 0.0]];
    for mutation in [
        IVFMutation::Replace {
            document: 2,
            vectors: &replacement,
        },
        IVFMutation::Replace {
            document: 2,
            vectors: &[],
        },
        IVFMutation::Delete(2),
        IVFMutation::Delete(100),
        IVFMutation::Clear,
        IVFMutation::Train,
    ] {
        let control = StorageReadControl::with_limit(1 << 20);
        let prepared = source.prepare_metadata(mutation, &control).unwrap();
        let mut expected = source.detached_clone();
        match mutation {
            IVFMutation::Replace { document, vectors } => {
                expected.add_many(document, vectors.to_vec()).unwrap();
            }
            IVFMutation::Delete(document) => expected.delete(document).unwrap(),
            IVFMutation::Clear => expected.clear().unwrap(),
            IVFMutation::Train => expected.train().unwrap(),
        }
        if expected.state() == IVFState::Stale {
            expected.train().unwrap();
        }
        assert_eq!(*prepared, expected.metadata_snapshot());
        assert_eq!(source.metadata_snapshot(), before);
        assert_eq!(control.memory().used(), prepared.reserved_bytes());
        drop(prepared);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn failed_candidate_returns_its_allowance_and_preserves_the_source() {
    let source = trained();
    let before = source.metadata_snapshot();
    let control = StorageReadControl::with_limit(1);
    let allocation = allocation_counter::measure(|| {
        assert!(matches!(
            source.prepare_metadata(IVFMutation::Train, &control),
            Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
        ));
    });
    assert_eq!(
        allocation.bytes_total, 0,
        "the allowance must reject the candidate before its corpus is cloned"
    );
    assert_eq!(control.memory().used(), 0);
    let empty = source
        .prepare_metadata(IVFMutation::Clear, &control)
        .unwrap();
    assert_eq!(empty.vector_count, 0);
    assert_eq!(control.memory().used(), 0);
    let control = StorageReadControl::with_limit(1 << 20);
    let invalid = [vec![f32::NAN; 3]];
    assert!(source
        .prepare_metadata(
            IVFMutation::Replace {
                document: 1,
                vectors: &invalid
            },
            &control
        )
        .is_err());
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        source.prepare_metadata(IVFMutation::Train, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(source.metadata_snapshot(), before);
}

#[test]
fn prepared_deletion_retrains_only_after_crossing_the_stale_threshold() {
    let mut source = trained();
    source.delete(1).unwrap();
    source.delete(2).unwrap();
    let before = source.metadata_snapshot();
    assert_eq!(before.deletes_since_train, 2);
    let control = StorageReadControl::with_limit(1 << 20);
    let empty = source
        .prepare_metadata(
            IVFMutation::Replace {
                document: 3,
                vectors: &[],
            },
            &control,
        )
        .unwrap();
    assert_eq!(empty.trained_size, 10);
    assert_eq!(empty.deletes_since_train, 2);
    assert_eq!(empty.centroids, before.centroids);
    let deleted = source
        .prepare_metadata(IVFMutation::Delete(3), &control)
        .unwrap();
    assert_eq!(deleted.trained_size, 7);
    assert_eq!(deleted.deletes_since_train, 0);
    assert_eq!(deleted.vector_count, 7);
    assert_eq!(source.metadata_snapshot(), before);
}

#[test]
fn restoration_rejects_missing_tensor_ordinals_and_accepts_unsorted_complete_tensors() {
    let mut index = IVFIndex::with_params(2, 2, 2, 100);
    index
        .add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    for ordinals in [[0, 2], [1, 2], [0, 0], [1, 0]] {
        let vectors = ordinals
            .into_iter()
            .map(|ordinal| (1, ordinal, vec![1.0, 0.0]))
            .collect();
        let restored = IVFIndex::from_persistence(2, 2, 2, 100, vectors, index.metadata_snapshot());
        assert_eq!(restored.is_ok(), ordinals == [1, 0]);
    }
}
