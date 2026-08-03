//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::DocId;

use super::{HNSWIndex, MAX_HNSW_LEVEL};
use crate::vector_index::{HNSWIndexParams, MemoryVectorIndex, VectorIndex};

fn vector(seed: u64, dimensions: usize) -> Vec<f32> {
    let mut state = seed;
    (0..dimensions)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 40) as f32 / (1_u32 << 24) as f32) * 2.0 - 1.0
        })
        .collect()
}

fn ids(list: &uqa_core::PostingList) -> Vec<DocId> {
    list.iter().map(|entry| entry.doc_id).collect()
}

#[test]
fn nearest_neighbor_and_graph_invariants_hold() {
    let mut index = HNSWIndex::new(4);
    for doc_id in 1..=128 {
        index.add(doc_id, vector(doc_id, 4)).unwrap();
    }
    index.validate_invariants().unwrap();
    let query = vector(73, 4);
    assert_eq!(ids(&index.search_knn(&query, 1).unwrap()), vec![73]);
}

#[test]
fn tensor_vectors_collapse_to_unique_documents() {
    let mut index = HNSWIndex::new(2);
    index
        .add_many(1, vec![vec![1.0, 0.0], vec![0.99, 0.01]])
        .unwrap();
    index.add(2, vec![0.8, 0.2]).unwrap();
    let result = index.search_knn(&[1.0, 0.0], 2).unwrap();
    assert_eq!(ids(&result), vec![1, 2]);
}

#[test]
fn replacement_and_tombstone_rebuild_preserve_results() {
    let params = HNSWIndexParams {
        rebuild_threshold: 1,
        ..HNSWIndexParams::default()
    };
    let mut index = HNSWIndex::with_params(2, params).unwrap();
    index.add(1, vec![1.0, 0.0]).unwrap();
    index.add(2, vec![0.0, 1.0]).unwrap();
    index.add(1, vec![-1.0, 0.0]).unwrap();
    index.validate_invariants().unwrap();
    assert_eq!(ids(&index.search_knn(&[1.0, 0.0], 1).unwrap()), vec![2]);
    index.delete(2).unwrap();
    assert_eq!(ids(&index.search_knn(&[1.0, 0.0], 1).unwrap()), vec![1]);
}

#[test]
fn persistence_snapshot_round_trips_without_rebuilding() {
    let params = HNSWIndexParams::default();
    let mut index = HNSWIndex::with_params(3, params).unwrap();
    for doc_id in 1..=24 {
        index.add(doc_id, vector(doc_id, 3)).unwrap();
    }
    let snapshot = index.persistence_snapshot();
    let mut restored =
        HNSWIndex::from_persistence(3, params, snapshot.meta, snapshot.nodes).unwrap();
    assert_eq!(
        ids(&index.search_knn(&vector(11, 3), 6).unwrap()),
        ids(&restored.search_knn(&vector(11, 3), 6).unwrap())
    );
    assert!(!restored.take_persistence_delta().full_rewrite);
}

#[test]
fn persistence_rejects_levels_above_the_allocation_bound() {
    let params = HNSWIndexParams::default();
    let mut index = HNSWIndex::with_params(2, params).unwrap();
    index.add(1, vec![1.0, 0.0]).unwrap();
    let mut snapshot = index.persistence_snapshot();
    snapshot.meta.max_level = MAX_HNSW_LEVEL + 1;
    let error = HNSWIndex::from_persistence(2, params, snapshot.meta, snapshot.nodes).unwrap_err();
    assert!(error.to_string().contains("supported maximum"));
}

#[test]
fn high_ef_search_reaches_bruteforce_recall_floor() {
    let params = HNSWIndexParams {
        m: 12,
        ef_construction: 96,
        ef_search: 128,
        ..HNSWIndexParams::default()
    };
    let mut hnsw = HNSWIndex::with_params(8, params).unwrap();
    let mut exact = MemoryVectorIndex::new(8);
    for doc_id in 1_u64..=256 {
        let value = vector(doc_id.wrapping_mul(17), 8);
        hnsw.add(doc_id, value.clone()).unwrap();
        exact.add(doc_id, value).unwrap();
    }
    let mut matched = 0_usize;
    let mut expected = 0_usize;
    for query_id in 300..320 {
        let query = vector(query_id, 8);
        let approximate = ids(&hnsw.search_knn(&query, 10).unwrap());
        let exhaustive = ids(&exact.search_knn(&query, 10).unwrap());
        expected += exhaustive.len();
        matched += exhaustive
            .iter()
            .filter(|doc_id| approximate.contains(doc_id))
            .count();
    }
    assert!(
        matched * 100 >= expected * 95,
        "recall={matched}/{expected}"
    );
}

#[test]
fn default_search_has_a_recall_floor_against_bruteforce() {
    let mut hnsw = HNSWIndex::new(12);
    let mut exact = MemoryVectorIndex::new(12);
    for doc_id in 1_u64..=512 {
        let value = vector(doc_id.wrapping_mul(31), 12);
        hnsw.add(doc_id, value.clone()).unwrap();
        exact.add(doc_id, value).unwrap();
    }
    let mut matched = 0_usize;
    let mut expected = 0_usize;
    for query_id in 1_000..1_020 {
        let query = vector(query_id, 12);
        let approximate = ids(&hnsw.search_knn(&query, 10).unwrap());
        let exhaustive = ids(&exact.search_knn(&query, 10).unwrap());
        expected += exhaustive.len();
        matched += exhaustive
            .iter()
            .filter(|doc_id| approximate.contains(doc_id))
            .count();
    }
    assert!(
        matched * 100 >= expected * 90,
        "default recall={matched}/{expected}"
    );
}

#[test]
fn repeated_vectors_keep_a_large_graph_connected_and_restorable() {
    let params = HNSWIndexParams::default();
    let mut index = HNSWIndex::with_params(2, params).unwrap();
    for doc_id in 1_u64..=2_000 {
        let value = if doc_id % 200 == 1 {
            vec![0.9, 0.1]
        } else {
            vec![0.1, 0.9]
        };
        index.add(doc_id, value).unwrap();
    }

    index.validate_invariants().unwrap();
    assert_eq!(index.search_knn(&[0.9, 0.1], 10).unwrap().len(), 10);

    let snapshot = index.persistence_snapshot();
    let restored = HNSWIndex::from_persistence(2, params, snapshot.meta, snapshot.nodes).unwrap();
    restored.validate_invariants().unwrap();
    assert_eq!(restored.search_knn(&[0.9, 0.1], 10).unwrap().len(), 10);
}

#[test]
fn non_finite_vectors_are_rejected_before_mutation() {
    let mut index = HNSWIndex::new(2);
    assert!(index.add(1, vec![f32::NAN, 0.0]).is_err());
    assert_eq!(index.count().unwrap(), 0);
}
