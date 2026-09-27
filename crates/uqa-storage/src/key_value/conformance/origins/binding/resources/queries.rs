//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Barrier;

use super::{expect, expect_eq, raw, DOCUMENTS};
use crate::{
    read_control::StorageReadControl,
    vector_index::{DiskANNExecutionRoute, VectorQueryResult},
    StorageBackendError, StorageBackendResult, VectorIndex,
};

pub(super) fn concurrent(
    indexes: [&dyn VectorIndex; 3],
    owner: &StorageReadControl,
) -> StorageBackendResult<()> {
    let start = Barrier::new(3);
    let invoking = std::array::from_fn::<_, 3, _>(|_| StorageReadControl::with_limit(0));
    let results = std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for (index, control) in indexes.into_iter().zip(&invoking) {
            let start = &start;
            workers.push(scope.spawn(move || {
                start.wait();
                index.search_knn_with_statistics(&raw(4), DOCUMENTS as usize, Some(control))
            }));
        }
        workers
            .into_iter()
            .map(|worker| {
                worker.join().map_err(|_| {
                    StorageBackendError::Other("concurrent DiskANN query worker panicked".into())
                })?
            })
            .collect::<StorageBackendResult<Vec<_>>>()
    })?;
    // Result construction is charged; completed PostingLists transfer to the caller under the shared vector-index contract.
    expect(
        owner.memory().used() <= owner.memory().limit(),
        "concurrent workspace respects the original allowance",
    )?;
    for (position, result) in results.iter().enumerate() {
        check(result, position != 0, position == 1)?;
    }
    for control in &invoking {
        expect_eq(
            &control.memory().used(),
            &0,
            "fresh invocation cannot replace retained budget",
        )?;
    }
    Ok(())
}

fn check(result: &VectorQueryResult, changed: bool, journal: bool) -> StorageBackendResult<()> {
    let expected: Vec<_> = (1..=DOCUMENTS + u64::from(changed))
        .filter(|&document| !changed || document != 2)
        .map(|document| {
            let score: f64 = if changed && (document == 1 || document == DOCUMENTS + 1) {
                1.0
            } else {
                match document % 4 {
                    0 => 1.0,
                    2 => -1.0,
                    _ => 0.0,
                }
            };
            (document, score.to_bits())
        })
        .collect();
    expect_eq(
        &result
            .postings
            .iter()
            .map(|entry| (entry.doc_id, entry.payload.score.to_bits()))
            .collect::<Vec<_>>(),
        &expected,
        "concurrent query preserves literal complete canonical results",
    )?;
    let report = result
        .diskann
        .as_ref()
        .expect("actual DiskANN query report");
    expect_eq(
        &report.route,
        &DiskANNExecutionRoute::Approximate,
        "bounded approximate query route",
    )?;
    expect(report.traversal.pq_estimates > 0, "actual PQ traversal")?;
    expect(
        report.work.pages.provider_pages > 0,
        "cold graph pages read from real provider",
    )?;
    expect(
        report.work.pages.cache_hits > 0,
        "small cache serves actual retained provider pages",
    )?;
    expect_eq(&report.work.exact.vectors, &0, "no implicit exact fallback")?;
    expect_eq(
        &report.work.changed.vectors,
        &(if journal { 2 } else { 0 }),
        "retained changed vectors scored separately",
    )
}

pub(super) fn rejection(
    index: &dyn VectorIndex,
    owner: &StorageReadControl,
) -> StorageBackendResult<()> {
    let invoking = StorageReadControl::with_limit(16 << 20);
    let full = owner
        .memory()
        .reserve(owner.memory().limit() - owner.memory().used())?;
    let rejected = index.search_knn_with_control(&raw(4), 1, &invoking);
    expect(
        matches!(rejected, Err(StorageBackendError::Memory(_))),
        "original exhausted allowance rejects query without borrowing a fresh budget",
    )?;
    drop(full);
    invoking.cancellation().cancel();
    expect(
        index
            .search_knn_with_control(&raw(4), 1, &invoking)
            .is_err(),
        "independent query cancellation is retained",
    )?;
    expect_eq(
        &index.search_knn(&raw(4), 1)?.len(),
        &1,
        "failed invocation leaves original reader usable",
    )?;
    expect_eq(
        &invoking.memory().used(),
        &0,
        "rejected invocation retains no fresh memory",
    )
}
