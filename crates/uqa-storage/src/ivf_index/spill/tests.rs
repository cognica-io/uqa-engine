//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Spilled generations preserve resident IVF arithmetic and mutation observations.

use super::*;
use crate::{IVFIndex, VectorIndex};

#[test]
fn corpus_larger_than_allowance_spills_and_preserves_training_and_document_replay() {
    const DIMENSIONS: usize = 128;
    const DOCUMENTS: usize = 400;
    let params = IVFIndexParams {
        nlist: 4,
        nprobe: 4,
        train_threshold: 16,
    };
    let mut expected = IVFIndex::with_params(DIMENSIONS as u32, 4, 4, 16);
    for document in 0..DOCUMENTS {
        let mut vector = vec![0.0; DIMENSIONS];
        vector[document % DIMENSIONS] = 1.0;
        vector[(document + 3) % DIMENSIONS] = 0.25;
        expected.add(document as u64, vector).unwrap();
    }
    expected.train().unwrap();
    let before = expected.metadata_snapshot();
    let control = StorageReadControl::with_limit(128 * 1024);
    assert!(DOCUMENTS * DIMENSIONS * 4 > control.memory().limit());
    let captured = restore(&expected, params, &control);
    assert!(captured.vectors.is_spilled());
    assert_eq!(captured.header().centroids, before.centroids);
    assert_eq!(
        captured
            .assignments()
            .collect::<StorageBackendResult<Vec<_>>>()
            .unwrap(),
        before.assignments
    );
    let retained = captured.vectors.clone();
    let replacement = [vec![1.0; DIMENSIONS], vec![0.5; DIMENSIONS]];
    expected.add_many(3, replacement.to_vec()).unwrap();
    let mut mutations = vec![IVFMutation::Replace {
        document: 3,
        vectors: &replacement,
    }];
    for document in 0..85 {
        mutations.push(IVFMutation::Delete(document));
        expected.delete(document).unwrap();
        if expected.state() == IVFState::Stale {
            expected.train().unwrap();
        }
    }
    let prepared = captured.prepare(&mutations).unwrap();
    assert!(prepared.vectors.is_spilled());
    let after = expected.metadata_snapshot();
    assert_eq!(prepared.header().centroids, after.centroids);
    assert_eq!(prepared.header().vector_count, after.vector_count);
    assert_eq!(prepared.header().trained_size, after.trained_size);
    assert_eq!(
        prepared.header().deletes_since_train,
        after.deletes_since_train
    );
    assert_eq!(
        prepared
            .assignments()
            .collect::<StorageBackendResult<Vec<_>>>()
            .unwrap(),
        after.assignments
    );
    assert_eq!(retained.len(), DOCUMENTS);
    assert!(retained.get(key(3, 0)).unwrap().is_some());
    assert!(retained.get(key(3, 1)).unwrap().is_none());
    assert!(control.memory().peak() <= control.memory().limit());
    let empty = [];
    expected.add_many(90, Vec::new()).unwrap();
    let prepared = prepared
        .prepare(&[IVFMutation::Replace {
            document: 90,
            vectors: &empty,
        }])
        .unwrap();
    assert_eq!(
        prepared.header().deletes_since_train,
        expected.metadata_snapshot().deletes_since_train
    );
    let replacement = vec![vec![1.0; DIMENSIONS]; 16];
    expected.clear().unwrap();
    expected.add_many(500, replacement.clone()).unwrap();
    let prepared = prepared
        .prepare(&[
            IVFMutation::Clear,
            IVFMutation::Replace {
                document: 500,
                vectors: &replacement,
            },
        ])
        .unwrap();
    let after = expected.metadata_snapshot();
    assert_eq!(prepared.header().centroids, after.centroids);
    assert_eq!(prepared.header().trained_size, after.trained_size);
    assert_eq!(
        prepared
            .assignments()
            .collect::<StorageBackendResult<Vec<_>>>()
            .unwrap(),
        after.assignments
    );
    drop((prepared, retained));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn streamed_reconstruction_rejects_corruption_quota_and_cancellation_without_retention() {
    let header = || IVFMetadataSnapshot {
        state: IVFState::Untrained,
        centroids: vec![],
        assignments: vec![],
        trained_size: 0,
        deletes_since_train: 0,
        vector_count: 1,
    };
    let params = IVFIndexParams {
        nlist: 2,
        nprobe: 2,
        train_threshold: 2,
    };
    let rejected = StorageReadControl::with_limit(1);
    assert!(IVFRestoreBuilder::new(2, params, header(), &rejected).is_err());
    assert_eq!(rejected.memory().used(), 0);
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut builder = IVFRestoreBuilder::new(2, params, header(), &control).unwrap();
    assert!(builder.vector(1, 1, &[1.0, 0.0]).is_err());
    builder.vector(1, 0, &[1.0, 0.0]).unwrap();
    let captured = builder.finish().unwrap();
    control.cancellation().cancel();
    assert!(captured.prepare(&[IVFMutation::Train]).is_err());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn streamed_reconstruction_preserves_invalid_vector_and_counter_diagnostics() {
    let params = IVFIndexParams {
        nlist: 2,
        nprobe: 2,
        train_threshold: 2,
    };
    let control = StorageReadControl::with_limit(64 * 1024);
    let header = IVFMetadataSnapshot {
        state: IVFState::Trained,
        centroids: vec![],
        assignments: vec![],
        trained_size: 1,
        deletes_since_train: usize::MAX,
        vector_count: 1,
    };
    let mut builder = IVFRestoreBuilder::new(2, params, header.clone(), &control).unwrap();
    let mut invalid = header.clone();
    invalid.centroids = vec![vec![f32::NAN, 0.0]];
    let expected = IVFIndex::from_persistence(2, 2, 2, 2, vec![(1, 0, vec![1.0, 0.0])], invalid)
        .err()
        .unwrap();
    assert_eq!(
        builder
            .centroid(0, &[f32::NAN, 0.0])
            .unwrap_err()
            .to_string(),
        expected.to_string()
    );
    builder.centroid(0, &[1.0, 0.0]).unwrap();
    builder.assignment(1, 0, 0).unwrap();
    let mut invalid = header;
    invalid.centroids = vec![vec![1.0, 0.0]];
    invalid.assignments = vec![(1, 0, 0)];
    let expected = IVFIndex::from_persistence(2, 2, 2, 2, vec![(1, 0, vec![1.0])], invalid)
        .err()
        .unwrap();
    assert_eq!(
        builder.vector(1, 0, &[1.0]).unwrap_err().to_string(),
        expected.to_string()
    );
    builder.vector(1, 0, &[1.0, 0.0]).unwrap();
    let captured = builder.finish().unwrap();
    let retained = captured.vectors.clone();
    let error = captured.prepare(&[IVFMutation::Delete(1)]).err().unwrap();
    assert_eq!(
        error.to_string(),
        "IVF deletes-since-train counter overflow"
    );
    assert!(matches!(error, StorageBackendError::Other(_)));
    assert!(retained.get(key(1, 0)).unwrap().is_some());
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}

fn restore(
    expected: &IVFIndex,
    params: IVFIndexParams,
    control: &StorageReadControl,
) -> IVFPreparedMetadata {
    let before = expected.metadata_snapshot();
    let mut header = before.clone();
    header.centroids.clear();
    header.assignments.clear();
    let mut builder = IVFRestoreBuilder::new(expected.dimensions, params, header, control).unwrap();
    for (id, centroid) in before.centroids.iter().enumerate() {
        builder.centroid(id, centroid).unwrap();
    }
    for &(document, ordinal, centroid) in &before.assignments {
        builder.assignment(document, ordinal, centroid).unwrap();
    }
    for vector in expected.vectors.lock().values() {
        builder
            .vector(vector.doc_id, vector.vector_ordinal, &vector.raw_vector)
            .unwrap();
    }
    builder.finish().unwrap()
}
