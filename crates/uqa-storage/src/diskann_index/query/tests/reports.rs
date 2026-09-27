//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    diskann_index::{pages::DiskANNPageReadStats, DiskANNScoringStats},
    vector_index::DiskANNExecutionRoute,
    ReadOnlySnapshot, StorageBackendError,
};

#[test]
fn diskann_vector_reports_preserve_one_invocation_through_nested_readers() {
    let source = Source::new([
        (1, vec![vec![1.0, 0.0], vec![-1.0, 0.0]]),
        (2, vec![vec![0.0, 0.0]]),
        (3, vec![]),
    ]);
    let control = StorageReadControl::with_limit(1 << 20);
    let physical = build(&source);
    let index =
        RetainedDiskANNIndex::open(source.clone(), physical, parameters(), limits(), &control)
            .unwrap();
    let generation = index.manifest().input().generation;
    let index: Arc<dyn VectorIndex> = Arc::new(index);
    let bound = StorageReadControl::with_limit(0);
    let readers: [Arc<dyn VectorIndex>; 3] = [
        index.clone(),
        Arc::new(ReadOnlySnapshot::new(index.clone())),
        ReadOnlySnapshot::new(index)
            .with_vector_read_control(&bound)
            .unwrap()
            .snapshot()
            .unwrap()
            .snapshot()
            .unwrap(),
    ];
    let invoking = StorageReadControl::with_limit(0);
    for reader in readers {
        for invocation in [None, Some(&invoking)] {
            source.reset();
            let result = reader
                .search_knn_with_statistics(&[1.0, 0.0], 99, invocation)
                .unwrap();
            assert_eq!(
                bits(&result.postings),
                [(1, 1.0_f64.to_bits()), (2, 0.0_f64.to_bits())]
            );
            let report = result.diskann.unwrap();
            assert_eq!(report.generation, generation);
            assert_eq!(report.route, DiskANNExecutionRoute::Approximate);
            assert_eq!(
                report.work.reranked,
                DiskANNScoringStats {
                    documents: 2,
                    vectors: 3
                }
            );
            assert_eq!(report.work.changed, DiskANNScoringStats::default());
            assert_eq!(report.work.side_entries, 1);
            assert_eq!(report.work.pages.page_requests, 2);
            assert_eq!(report.work.pages.provider_pages, 2);
            assert_eq!(
                report.traversal.approximate_expansions + report.traversal.completion_expansions,
                2
            );
            let visits = source.visits.lock().unwrap();
            assert_eq!(visits.get(&1), Some(&1));
            assert_eq!(visits.get(&2), Some(&1));
            assert!(
                visits.values().all(|&count| count == 1),
                "a report must not replay scoring"
            );
            drop(visits);

            exact_reports(&*reader, &source, generation, invocation);
        }
    }
    assert_eq!(bound.memory().used(), 0);
    assert_eq!(invoking.memory().used(), 0);
}

#[test]
fn diskann_vector_reports_preserve_original_budget_and_independent_cancellation() {
    let source = Source::new([(1, vec![vec![1.0, 0.0]])]);
    let physical = build(&source);
    let original = StorageReadControl::with_limit(1 << 20);
    let index =
        RetainedDiskANNIndex::open(source, physical, parameters(), limits(), &original).unwrap();
    let bound = StorageReadControl::with_limit(0);
    let index = ReadOnlySnapshot::new(Arc::new(index))
        .with_vector_read_control(&bound)
        .unwrap();
    let invoking = StorageReadControl::with_limit(1 << 20);
    let held = original
        .memory()
        .reserve(original.memory().limit() - original.memory().used())
        .unwrap();
    assert!(matches!(
        index.search_knn_with_statistics(&[1.0, 0.0], 1, Some(&invoking)),
        Err(StorageBackendError::Memory(_))
    ));
    assert!(matches!(
        index.search_threshold_with_statistics(&[1.0, 0.0], 0.0, Some(&invoking)),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(invoking.memory().used(), 0);
    drop(held);
    for cancellation in [&invoking, &bound, &original] {
        cancellation.cancellation().cancel();
        assert!(matches!(
            index.search_knn_with_statistics(&[1.0, 0.0], 0, Some(&invoking)),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert!(matches!(
            index.search_threshold_with_statistics(&[1.0, 0.0], 0.0, Some(&invoking)),
            Err(StorageBackendError::Cancelled(_))
        ));
        cancellation.cancellation().reset();
    }
    assert_eq!(
        index
            .search_knn_with_statistics(&[1.0, 0.0], 1, Some(&invoking))
            .unwrap()
            .postings
            .len(),
        1
    );
}

fn exact_reports(
    reader: &dyn VectorIndex,
    source: &Source,
    generation: DiskANNGeneration,
    invocation: Option<&StorageReadControl>,
) {
    for (raw, route) in [
        ([0.0, 0.0], DiskANNExecutionRoute::ExactZeroNorm),
        (
            [f32::MAX, f32::MAX],
            DiskANNExecutionRoute::ExactNonFiniteNorm,
        ),
    ] {
        source.reset();
        let result = reader
            .search_knn_with_statistics(&raw, 1, invocation)
            .unwrap();
        assert_eq!(result.postings.len(), 1);
        let report = result.diskann.unwrap();
        assert_eq!(report.route, route);
        assert_eq!(
            report.work.exact,
            DiskANNScoringStats {
                documents: 2,
                vectors: 3
            }
        );
        assert_eq!(report.work.pages, DiskANNPageReadStats::default());
        assert!(source
            .visits
            .lock()
            .unwrap()
            .values()
            .all(|&count| count == 1));
    }
    source.reset();
    let result = reader
        .search_threshold_with_statistics(&[1.0, 0.0], 1.0, invocation)
        .unwrap();
    assert_eq!(bits(&result.postings), [(1, 1.0_f64.to_bits())]);
    let report = result.diskann.unwrap();
    assert_eq!(report.route, DiskANNExecutionRoute::ExactThreshold);
    assert_eq!(report.generation, generation);
    assert_eq!(
        report.work.exact,
        DiskANNScoringStats {
            documents: 2,
            vectors: 3
        }
    );
    assert_eq!(report.traversal, DiskANNTraversalStats::default());
    assert_eq!(source.visits.lock().unwrap().len(), 3);
    assert!(source
        .visits
        .lock()
        .unwrap()
        .values()
        .all(|&count| count == 1));
    source.reset();
    let result = reader
        .search_knn_with_statistics(&[0.0, 0.0], 0, invocation)
        .unwrap();
    assert!(result.postings.is_empty());
    let report = result.diskann.unwrap();
    assert_eq!(report.route, DiskANNExecutionRoute::EmptyK);
    assert_eq!(report.work, DiskANNQueryWork::default());
    assert_eq!(report.traversal, DiskANNTraversalStats::default());
    assert!(source.visits.lock().unwrap().is_empty());
    assert!(reader
        .search_knn_with_statistics(&[1.0], 0, invocation)
        .is_err());
    assert!(reader
        .search_threshold_with_statistics(&[1.0, 0.0], f32::NAN, invocation)
        .is_err());
}
