//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{hnsw_index::HNSWIndex, vector_index::VectorIndex, StorageBackendError};

#[test]
fn canonical_replacement_comparison_preserves_bits_ordinals_and_empty_tensors() {
    let mut source = HNSWIndex::new(2);
    let vectors = vec![vec![1.0, -0.0], vec![0.0, 2.0]];
    source.add_many(7, vectors.clone()).unwrap();
    let control = StorageReadControl::with_limit(4096);
    assert!(canonical_vectors_equal(&source, 7, &vectors, &control).unwrap());
    assert!(
        !canonical_vectors_equal(&source, 7, &[vec![1.0, 0.0], vec![0.0, 2.0]], &control).unwrap()
    );
    assert!(
        !canonical_vectors_equal(&source, 7, &[vec![2.0, -0.0], vec![0.0, 2.0]], &control).unwrap()
    );
    assert!(!canonical_vectors_equal(
        &source,
        7,
        &[vectors[1].clone(), vectors[0].clone()],
        &control
    )
    .unwrap());
    assert!(!canonical_vectors_equal(&source, 7, &vectors[..1], &control).unwrap());
    assert!(!canonical_vectors_equal(&source, 7, &[], &control).unwrap());
    assert!(!canonical_vectors_equal(&source, 8, &vectors, &control).unwrap());
    assert!(canonical_vectors_equal(&source, 8, &[], &control).unwrap());
    for invalid in [vec![1.0], vec![f32::NAN, 0.0], vec![f32::INFINITY, 0.0]] {
        assert!(canonical_vectors_equal(&source, 7, &[invalid], &control).is_err());
    }
    source.delete(7).unwrap();
    assert!(canonical_vectors_equal(&source, 7, &[], &control).unwrap());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn canonical_replacement_comparison_retains_only_one_vector_and_honors_control() {
    let vectors = vec![vec![1.0; 1024]; 12];
    for mut source in [
        Box::new(HNSWIndex::new(1024)) as Box<dyn VectorIndex>,
        Box::new(crate::ivf_index::IVFIndex::new(1024)),
    ] {
        source.add_many(1, vectors.clone()).unwrap();
        let control = StorageReadControl::with_limit(1024 * size_of::<f32>());
        assert!(source
            .matches_document_vectors(1, &vectors, &control)
            .unwrap());
        assert_eq!(control.memory().peak(), control.memory().limit());
        assert_eq!(control.memory().used(), 0);
        let occupied = control.memory().reserve(1).unwrap();
        assert!(matches!(
            source.matches_document_vectors(1, &vectors, &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), 1);
        drop(occupied);
        control.cancellation().cancel();
        assert!(matches!(
            source.matches_document_vectors(1, &vectors, &control),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(control.memory().used(), 0);
    }
}
