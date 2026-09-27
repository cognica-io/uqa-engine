//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    expect, expect_eq, DiskANNGeneration, StorageBackendResult, StorageReadControl, VectorIndex,
};
use crate::{
    diskann_index::{
        pages::DiskANNPageReadStats, search::DiskANNTraversalStats, DiskANNQueryWork,
        DiskANNScoringStats,
    },
    vector_index::DiskANNExecutionRoute,
};

/// Check one disposable fixture's literal document/vector work through its actual runtime or retained handle. The pairs are successful tensor scores and their vector counts; no timing or physical-I/O assertion is made.
pub fn verify_diskann_vector_statistics(
    index: &dyn VectorIndex,
    generation: DiskANNGeneration,
    reranked: (u64, u64),
    changed: (u64, u64),
) -> StorageBackendResult<()> {
    let invoking = StorageReadControl::with_limit(0);
    for control in [None, Some(&invoking)] {
        let result = index.search_knn_with_statistics(&[1.0, 0.0], 99, control)?;
        let report = result.diskann.expect("DiskANN runtime report");
        expect_eq(
            &report.generation,
            &generation,
            "report uses the actual selected generation",
        )?;
        expect_eq(
            &report.route,
            &DiskANNExecutionRoute::Approximate,
            "ordinary KNN route",
        )?;
        expect_eq(
            &report.work.reranked,
            &DiskANNScoringStats {
                documents: reranked.0,
                vectors: reranked.1,
            },
            "complete covered tensor work",
        )?;
        expect_eq(
            &report.work.changed,
            &DiskANNScoringStats {
                documents: changed.0,
                vectors: changed.1,
            },
            "complete changed tensor work",
        )?;
        expect_eq(
            &report.work.unversioned,
            &DiskANNScoringStats::default(),
            "actual origins require no raw scores",
        )?;
        expect_eq(
            &report.work.exact,
            &DiskANNScoringStats::default(),
            "approximate route has no exact scan",
        )?;
        let pages = report.work.pages;
        expect_eq(
            &pages.page_requests,
            &(pages.cache_hits + pages.provider_pages),
            "actual page partition",
        )?;
        expect(
            pages.provider_batches <= pages.provider_pages,
            "nonempty provider batches",
        )?;
        expect_eq(
            &(result.postings.len() as u64),
            &(reranked.0 + changed.0),
            "complete fixture support",
        )?;
        let exact = DiskANNScoringStats {
            documents: reranked.0 + changed.0,
            vectors: reranked.1 + changed.1,
        };
        verify_exact_and_empty(index, generation, exact, control)?;
    }
    expect_eq(
        &invoking.memory().used(),
        &0,
        "invocation never replaces original allowance",
    )
}

fn verify_exact_and_empty(
    index: &dyn VectorIndex,
    generation: DiskANNGeneration,
    exact: DiskANNScoringStats,
    control: Option<&StorageReadControl>,
) -> StorageBackendResult<()> {
    for (query, route) in [
        ([0.0, 0.0], DiskANNExecutionRoute::ExactZeroNorm),
        (
            [f32::MAX, f32::MAX],
            DiskANNExecutionRoute::ExactNonFiniteNorm,
        ),
    ] {
        let result = index.search_knn_with_statistics(&query, 1, control)?;
        let report = result.diskann.expect("exact KNN report");
        expect_eq(
            &report.generation,
            &generation,
            "exact query retains its generation",
        )?;
        expect_eq(&report.route, &route, "actual exact numeric route")?;
        expect_eq(
            &report.work.exact,
            &exact,
            "exact count precedes top-k reduction",
        )?;
        expect_eq(
            &report.work.pages,
            &DiskANNPageReadStats::default(),
            "exact search reads no graph pages",
        )?;
        expect_eq(
            &report.traversal,
            &DiskANNTraversalStats::default(),
            "exact search performs no traversal",
        )?;
    }
    let result = index.search_threshold_with_statistics(&[1.0, 0.0], 2.0, control)?;
    expect(
        result.postings.is_empty(),
        "threshold filters every cosine result",
    )?;
    let report = result.diskann.expect("threshold report");
    expect_eq(
        &report.generation,
        &generation,
        "threshold retains its generation",
    )?;
    expect_eq(
        &report.route,
        &DiskANNExecutionRoute::ExactThreshold,
        "threshold is always exact",
    )?;
    expect_eq(
        &report.work.exact,
        &exact,
        "threshold counts before result filtering",
    )?;
    let result = index.search_knn_with_statistics(&[0.0, 0.0], 0, control)?;
    let report = result.diskann.expect("zero-k report");
    expect_eq(
        &report.route,
        &DiskANNExecutionRoute::EmptyK,
        "zero-k performs no numeric search",
    )?;
    expect_eq(
        &report.work,
        &DiskANNQueryWork::default(),
        "zero-k does no work",
    )?;
    expect(result.postings.is_empty(), "zero-k support is empty")?;
    Ok(())
}
