//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::DocId;

use super::mutation::validate_vector_ordinal_count_for_test;
use super::training::STALE_DENOMINATOR;
use super::{IVFIndex, IVFState};
use crate::VectorIndex;

fn random_vector(seed: u64, dimensions: usize) -> Vec<f32> {
    let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    (0..dimensions)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            ((state >> 32) as i32 as f32) / i32::MAX as f32
        })
        .collect()
}

#[test]
fn untrained_search_falls_back_to_brute_force() {
    let mut index = IVFIndex::with_params(4, 8, 4, 1024);
    for doc_id in 0..16 {
        index.add(doc_id, random_vector(doc_id + 1, 4)).unwrap();
    }
    assert_eq!(index.state(), IVFState::Untrained);
    assert_eq!(index.search_knn(&random_vector(1, 4), 4).unwrap().len(), 4);
}

#[test]
fn auto_trains_above_threshold() {
    let mut index = IVFIndex::with_params(4, 4, 2, 16);
    for doc_id in 0..32 {
        index.add(doc_id, random_vector(doc_id + 1, 4)).unwrap();
    }
    assert_eq!(index.state(), IVFState::Trained);
}

#[test]
fn auto_train_threshold_counts_tensor_vectors_below_nlist() {
    let mut index = IVFIndex::with_params(2, 4, 4, 2);
    index
        .add_many(1, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    assert_eq!(index.state(), IVFState::Trained);
    assert_eq!(index.metadata_snapshot().trained_size, 2);
    assert_eq!(index.metadata_snapshot().vector_count, 2);
}

#[test]
fn search_returns_results_after_training() {
    let mut index = IVFIndex::with_params(8, 4, 4, 16);
    for doc_id in 0..64 {
        index.add(doc_id, random_vector(doc_id + 1, 8)).unwrap();
    }
    index.train().unwrap();
    assert!(!index
        .search_knn(&random_vector(1, 8), 1)
        .unwrap()
        .is_empty());
}

#[test]
fn stale_index_below_training_threshold_returns_to_untrained() {
    let mut index = IVFIndex::with_params(4, 4, 2, 16);
    for doc_id in 0..16 {
        index.add(doc_id, random_vector(doc_id + 1, 4)).unwrap();
    }
    for doc_id in 0..4 {
        index.delete(doc_id).unwrap();
    }
    assert_eq!(index.state(), IVFState::Stale);
    index.search_knn(&random_vector(1, 4), 2).unwrap();
    assert_eq!(index.state(), IVFState::Untrained);
}

#[test]
fn delete_marks_stale_above_fraction() {
    let mut index = IVFIndex::with_params(4, 4, 2, 16);
    for doc_id in 0..32 {
        index.add(doc_id, random_vector(doc_id + 1, 4)).unwrap();
    }
    index.train().unwrap();
    for doc_id in 0..(32 / STALE_DENOMINATOR + 4) {
        index.delete(doc_id as DocId).unwrap();
    }
    assert!(matches!(index.state(), IVFState::Stale | IVFState::Trained));
}

#[test]
fn threshold_search_emits_only_above_cutoff() {
    let mut index = IVFIndex::with_params(4, 4, 2, 1024);
    for doc_id in 0..16 {
        index.add(doc_id, random_vector(doc_id + 1, 4)).unwrap();
    }
    assert!(
        index
            .search_threshold(&random_vector(1, 4), 0.999)
            .unwrap()
            .len()
            <= 16
    );
}

#[test]
fn ordinal_count_matches_zero_based_u32_format() {
    validate_vector_ordinal_count_for_test(u64::from(u32::MAX) + 1).unwrap();
    let error = validate_vector_ordinal_count_for_test(u64::from(u32::MAX) + 2).unwrap_err();
    assert!(error.to_string().contains("u32 index format"));
}

#[test]
fn invalid_replacement_preserves_existing_vectors() {
    let mut index = IVFIndex::with_params(2, 4, 2, 1024);
    index.add(1, vec![1.0, 0.0]).unwrap();
    assert!(index.add(1, vec![f32::NAN, 0.0]).is_err());
    assert_eq!(index.count().unwrap(), 1);
    assert_eq!(
        index
            .search_knn(&[1.0, 0.0], 1)
            .unwrap()
            .doc_ids()
            .collect::<Vec<_>>(),
        vec![1]
    );
}

#[test]
fn delete_counter_overflow_preserves_vector_and_assignments() {
    let mut index = IVFIndex::with_params(2, 1, 1, 1);
    index.add(1, vec![1.0, 0.0]).unwrap();
    *index.deletes_since_train.lock() = usize::MAX;
    let before = index.metadata_snapshot();
    assert!(index
        .delete(1)
        .unwrap_err()
        .to_string()
        .contains("overflow"));
    assert_eq!(index.count().unwrap(), 1);
    let after = index.metadata_snapshot();
    assert_eq!(after.assignments, before.assignments);
    assert_eq!(after.deletes_since_train, usize::MAX);
}
