//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fixture, Keys, Kind, MAX_RECORD};
use crate::diskann_index::{format::DiskANNGeneration, pages::DiskANNRecordKey};
use crate::key_value::conformance::{expect, expect_eq};
use crate::key_value::{DiskANNStageStatus, KeyValueDiskANNStore};
use crate::read_control::StorageReadControl;
use crate::{KeyValueStore, StorageBackendResult};
use std::sync::Arc;

/// An explicitly discarded frozen build remains resumable with payloads larger than cleanup's allowance. Returns a partly reclaimed generation for actual cold reopen.
pub fn verify_diskann_reclamation_bounds(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<DiskANNGeneration> {
    let control = StorageReadControl::with_limit(1 << 20);
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    repository.initialize(&control)?;
    let mut stage = repository.allocate_stage(11, 12, &control)?;
    stage.start(&control)?;
    let generation = stage.generation();
    let keys = Keys::new(generation);
    let payload = vec![23; 32_768];
    store.with_mutation(&mut |_, batch| {
        batch.require_unchanged(keys.key(Kind::State).as_ref())?;
        for position in 0..70 {
            batch.put(
                keys.key(Kind::Record(DiskANNRecordKey::Codes(position)))
                    .as_ref(),
                &payload,
            )?;
        }
        Ok(())
    })?;
    expect(
        repository
            .reclaim_retired_step(generation, 64, &control)
            .is_err(),
        "a live writable build has no retirement authority",
    )?;
    let fixture = fixture(generation, &control)?;
    expect(
        stage.seal(fixture.manifest, MAX_RECORD, &control).is_err(),
        "the deliberately incomplete artifact cannot seal",
    )?;
    expect_eq(
        &stage.status(&control)?,
        &Some(DiskANNStageStatus::Frozen),
        "failed seal stays frozen",
    )?;
    expect(
        repository
            .reclaim_retired_step(generation, 64, &control)
            .is_err(),
        "a frozen build is not abandoned merely because it failed verification",
    )?;
    let held = store.open_retained_read_session(control.cancellation())?;
    expect(
        !stage.discard_step(1, &control)?,
        "owner explicitly starts bounded discard",
    )?;
    expect_eq(
        &store
            .scan_prefix_keys_after(keys.prefix(), None, 100)?
            .len(),
        &71,
        "one manifest removed while all code records remain",
    )?;
    let cleanup = StorageReadControl::with_limit(65_536);
    expect(
        !repository.reclaim_retired_step(generation, usize::MAX, &cleanup)?,
        "caller limits above 64 do not expand a cleanup batch",
    )?;
    expect_eq(
        &store
            .scan_prefix_keys_after(keys.prefix(), None, 100)?
            .len(),
        &7,
        "exactly 64 payload records removed and state retained",
    )?;
    expect_eq(
        &cleanup.memory().used(),
        &0,
        "cleanup releases its bounded key workspace",
    )?;
    store.vacuum()?;
    expect_eq(
        &held.get(keys.key(Kind::Record(DiskANNRecordKey::Codes(0))).as_ref())?,
        &Some(payload),
        "retained payload survives cleanup and history reclamation",
    )?;
    Ok(generation)
}

/// Complete the bounded discard after reopening the physical provider; no generation number can be reused.
pub fn verify_diskann_reclamation_reopen(
    store: &Arc<dyn KeyValueStore>,
    generation: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(65_536);
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    expect_eq(
        &repository
            .resume_stage(generation, &control)?
            .status(&control)?,
        &Some(DiskANNStageStatus::Discarding),
        "partial cleanup survives cold reopen",
    )?;
    expect(
        repository.reclaim_retired_step(generation, 64, &control)?,
        "resumed cleanup finishes",
    )?;
    expect(
        repository.reclaim_retired_step(generation, 64, &control)?,
        "completed cleanup is idempotent",
    )?;
    expect(
        store
            .scan_prefix_keys_after(Keys::new(generation).prefix(), None, 1)?
            .is_empty(),
        "reclaimed generation has no current records",
    )?;
    store.vacuum()
}
