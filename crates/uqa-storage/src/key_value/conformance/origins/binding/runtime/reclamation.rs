//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{generation, retirement, runtime, scores};
use crate::diskann_index::{
    build::DiskANNTemporaryBudget, format::DiskANNGeneration, pages::DiskANNPageSource,
};
use crate::key_value::conformance::{expect, expect_eq};
use crate::key_value::{DiskANNStageStatus, KeyValueDiskANNStore};
use crate::read_control::StorageReadControl;
use crate::{KeyValueStore, StorageBackendResult};
use std::sync::Arc;

/// Reclaim a real retired SQL generation while its committed or private query snapshot remains live. Use a fresh disposable provider.
pub fn verify_diskann_runtime_reclamation(
    store: &Arc<dyn KeyValueStore>,
    private: bool,
) -> StorageBackendResult<(DiskANNGeneration, DiskANNGeneration)> {
    let ((first, replacement), held) = retirement::retire_and_recreate(store, private)?;
    let control = StorageReadControl::with_limit(1 << 20);
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    let physical = repository.open_source(first, &control)?;
    expect(
        repository
            .reclaim_retired_step(replacement, 1, &control)
            .is_err(),
        "the selected generation cannot be reclaimed",
    )?;
    expect(
        repository.reclaim_retired_step(first, 0, &control).is_err(),
        "reclamation requires a nonzero bound",
    )?;
    expect(
        repository
            .reclaim_retired_step(first, 1, &StorageReadControl::with_limit(0))
            .is_err(),
        "zero allowance cannot reclaim a payload page",
    )?;
    let cancelled = StorageReadControl::with_limit(8192);
    cancelled.cancellation().cancel();
    expect(
        repository
            .reclaim_retired_step(first, 1, &cancelled)
            .is_err(),
        "cancelled reclamation leaves the generation unchanged",
    )?;
    expect_eq(
        &repository.resume_stage(first, &control)?.status(&control)?,
        &Some(DiskANNStageStatus::Retired),
        "rejected attempts preserve committed retirement",
    )?;
    expect(
        !repository.reclaim_retired_step(first, 1, &control)?,
        "one-record cleanup remains resumable",
    )?;
    expect_eq(
        &repository.resume_stage(first, &control)?.status(&control)?,
        &Some(DiskANNStageStatus::Discarding),
        "partial cleanup durably fences reopening",
    )?;
    store.vacuum()?;
    scores(&*held, &[(1, 1.0), (2, 0.0)])?;
    drop(repository);
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    let mut complete = false;
    for _ in 0..32 {
        complete = repository.reclaim_retired_step(first, 1, &control)?;
        if complete {
            break;
        }
    }
    expect(complete, "bounded cleanup reaches completion")?;
    expect(
        repository.reclaim_retired_step(first, 1, &control)?,
        "completed cleanup is idempotent",
    )?;
    store.vacuum()?;
    scores(&*held, &[(1, 1.0), (2, 0.0)])?;
    physical.read_graph_pages(&[0], &control, &mut |_, bytes| {
        expect_eq(
            &bytes.len(),
            &4096,
            "uncached old graph page remains readable",
        )
    })?;
    drop(physical);
    drop(held);
    store.vacuum()?;
    verify_diskann_runtime_reclaimed_reopen(store, (first, replacement))?;
    Ok((first, replacement))
}

/// The retired generation stays absent after all original owners close; the replacement and canonical tensors remain queryable.
pub fn verify_diskann_runtime_reclaimed_reopen(
    store: &Arc<dyn KeyValueStore>,
    generations: (DiskANNGeneration, DiskANNGeneration),
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    expect(
        repository.resume_stage(generations.0, &control).is_err(),
        "reclaimed generations cannot resume",
    )?;
    expect(
        repository.open_source(generations.0, &control).is_err(),
        "reclaimed generations cannot be reopened",
    )?;
    expect_eq(
        &generation(store, &control)?,
        &generations.1,
        "current head survives reclamation",
    )?;
    scores(
        &runtime(store, &DiskANNTemporaryBudget::new(1 << 20), &control)?,
        &[(1, 1.0), (2, 0.0)],
    )
}
