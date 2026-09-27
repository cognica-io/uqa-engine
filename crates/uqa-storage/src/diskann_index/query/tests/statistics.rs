//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::diskann_index::DiskANNScoringStats;

#[test]
fn diskann_query_work_separates_reranking_changes_and_numeric_side_entries() {
    let mut source = Source::new([
        (1, vec![vec![1.0, 0.0], vec![-1.0, 0.0]]),
        (2, vec![vec![0.0, 1.0]]),
        (3, vec![vec![0.0, 0.0]]),
        (4, vec![]),
    ]);
    let physical = build(&source);
    source.replace(2, vec![vec![-1.0, 0.0], vec![0.0, -1.0]]);
    source.replace(5, vec![vec![1.0, 0.0]]);
    source.replace(6, vec![]);
    let control = StorageReadControl::with_limit(1 << 20);
    let query = DiskANNQuery::open(&source, physical, parameters(), limits(), &control).unwrap();
    for _ in 0..2 {
        source.reset();
        let result = query.search_knn(&[1.0, 0.0], 10, &control).unwrap();
        assert_eq!(
            bits(&result.postings),
            [
                (1, 1.0_f64.to_bits()),
                (2, 0.0_f64.to_bits()),
                (3, 0.0_f64.to_bits()),
                (5, 1.0_f64.to_bits())
            ]
        );
        assert_eq!(
            result.work,
            DiskANNQueryWork {
                pages: crate::diskann_index::pages::DiskANNPageReadStats {
                    page_requests: 3,
                    cache_hits: 0,
                    provider_pages: 3,
                    provider_batches: 3,
                },
                side_entries: 1,
                reranked: DiskANNScoringStats {
                    documents: 2,
                    vectors: 3
                },
                changed: DiskANNScoringStats {
                    documents: 2,
                    vectors: 3
                },
                ..DiskANNQueryWork::default()
            }
        );
        assert_eq!(
            result.traversal.approximate_expansions + result.traversal.completion_expansions,
            3
        );
        for document in [1, 2, 3, 5] {
            assert_eq!(source.visits.lock().unwrap().get(&document), Some(&1));
        }
    }
    source.reset();
    let empty = query.search_knn(&[1.0, 0.0], 0, &control).unwrap();
    assert!(empty.postings.is_empty());
    assert_eq!(empty.work, DiskANNQueryWork::default());
    assert_eq!(empty.traversal, DiskANNTraversalStats::default());
    assert!(source.visits.lock().unwrap().is_empty());
}

#[test]
fn diskann_query_work_counts_exact_tensors_before_candidate_and_threshold_filtering() {
    let source = Source::new([
        (1, vec![vec![1.0, 0.0], vec![0.0, 1.0]]),
        (2, vec![vec![-1.0, 0.0]]),
        (3, vec![]),
        (4, vec![vec![0.0, 0.0]]),
    ]);
    let physical = build(&source);
    let control = StorageReadControl::with_limit(1 << 20);
    let query = DiskANNQuery::open(&source, physical, parameters(), limits(), &control).unwrap();
    let expected = DiskANNQueryWork {
        exact: DiskANNScoringStats {
            documents: 3,
            vectors: 4,
        },
        ..DiskANNQueryWork::default()
    };
    for raw in [[0.0, 0.0], [f32::MAX, f32::MAX]] {
        source.reset();
        let result = query.search_knn(&raw, 1, &control).unwrap();
        assert_eq!(result.postings.len(), 1);
        assert!(result.exact_reason.is_some());
        assert_eq!(result.work, expected);
        assert_eq!(result.traversal, DiskANNTraversalStats::default());
        for document in [1, 2, 3, 4] {
            assert_eq!(source.visits.lock().unwrap().get(&document), Some(&1));
        }
    }
    source.reset();
    let result = query
        .search_threshold_with_stats(&[1.0, 0.0], 1.0, &control)
        .unwrap();
    assert_eq!(bits(&result.postings), [(1, 1.0_f64.to_bits())]);
    assert_eq!(result.work, expected);
    assert_eq!(result.traversal, DiskANNTraversalStats::default());
    for document in [1, 2, 3, 4] {
        assert_eq!(source.visits.lock().unwrap().get(&document), Some(&1));
    }
}

#[test]
fn diskann_query_work_attributes_raw_definition_differences_without_inventing_origins() {
    let actual = Source::new([
        (1, vec![vec![1.0, 0.0]]),
        (2, vec![vec![-1.0, 0.0]]),
        (3, vec![vec![0.0, 0.0]]),
    ]);
    let physical = build(&actual);
    let control = StorageReadControl::with_limit(1 << 20);
    let index =
        RetainedDiskANNIndex::open(actual, physical.clone(), parameters(), limits(), &control)
            .unwrap();
    let mut raw = MemoryVectorIndex::new(2);
    raw.add(1, vec![0.0, 1.0]).unwrap();
    raw.add(2, vec![-1.0, 0.0]).unwrap();
    raw.add_many(4, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
        .unwrap();
    let retained = raw.snapshot_with_control(&control).unwrap();
    let canonical = matching::MatchingCanonical::new(
        index.diskann_read_snapshot(&control).unwrap().unwrap(),
        retained.vector_read_snapshot(&control).unwrap().unwrap(),
        &control,
    )
    .unwrap();
    let query = DiskANNQuery::open(&canonical, physical, parameters(), limits(), &control).unwrap();
    let result = query.search_knn(&[1.0, 0.0], 10, &control).unwrap();
    assert_eq!(
        bits(&result.postings),
        [
            (1, 0.0_f64.to_bits()),
            (2, (-1.0_f64).to_bits()),
            (4, 1.0_f64.to_bits())
        ]
    );
    assert_eq!(
        result.work,
        DiskANNQueryWork {
            pages: crate::diskann_index::pages::DiskANNPageReadStats {
                page_requests: 2,
                cache_hits: 0,
                provider_pages: 2,
                provider_batches: 2,
            },
            side_entries: 1,
            reranked: DiskANNScoringStats {
                documents: 1,
                vectors: 1
            },
            unversioned: DiskANNScoringStats {
                documents: 2,
                vectors: 3
            },
            ..DiskANNQueryWork::default()
        }
    );
    let expected = DiskANNQueryWork {
        exact: DiskANNScoringStats {
            documents: 3,
            vectors: 4,
        },
        ..DiskANNQueryWork::default()
    };
    let zero = query.search_knn(&[0.0, 0.0], 1, &control).unwrap();
    assert_eq!(zero.work, expected);
    assert_eq!(zero.traversal, DiskANNTraversalStats::default());
    let threshold = query
        .search_threshold_with_stats(&[1.0, 0.0], 1.0, &control)
        .unwrap();
    assert_eq!(bits(&threshold.postings), [(4, 1.0_f64.to_bits())]);
    assert_eq!(threshold.work, expected);
    assert_eq!(threshold.traversal, DiskANNTraversalStats::default());
}
