//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Keys, Kind};
use crate::diskann_index::pages::DiskANNRecordKey;
use crate::key_value::conformance::{expect, expect_eq};
use crate::key_value::{
    DiskANNMaintenanceStatus, DiskANNMaintenanceStep, KeyValueDiskANNMaintenance,
    KeyValueDiskANNStage, KeyValueDiskANNStore,
};
use crate::read_control::StorageReadControl;
use crate::{KeyValueStore, StorageBackendResult};
use std::sync::Arc;

/// A finite discovery boundary excludes later builds, skips live owners and deletes bounded key pages without materializing large payloads. Use a fresh disposable provider.
pub fn verify_diskann_maintenance(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    repository.initialize(&control)?;
    let abandoned = stage(store, &repository, 100, 70, &control)?;
    let generation = abandoned.generation();
    drop(abandoned);
    let live = stage(store, &repository, 200, 1, &control)?;
    let tail = stage(store, &repository, 400, 1, &control)?.generation();
    let keys = Keys::new(generation);
    let payload_key = keys.key(Kind::Record(DiskANNRecordKey::Codes(0)));
    let held = store.open_retained_read_session(control.cancellation())?;
    let cleanup = StorageReadControl::with_limit(65_536);
    let mut pass = KeyValueDiskANNMaintenance::start(store, &cleanup)?;
    let earlier = stage(store, &repository, 50, 1, &control)?.generation();
    let later = stage(store, &repository, 500, 1, &control)?.generation();
    for status in [
        DiskANNMaintenanceStatus::MoreRecords,
        DiskANNMaintenanceStatus::Reclaimed,
    ] {
        expect_eq(
            &pass.step()?,
            &Some(DiskANNMaintenanceStep { generation, status }),
            "large abandoned generation uses two bounded batches",
        )?;
        if status == DiskANNMaintenanceStatus::MoreRecords {
            expect_eq(
                &store
                    .scan_prefix_keys_after(keys.prefix(), None, 100)?
                    .len(),
                &7,
                "first maintenance step deletes exactly 64 payload keys",
            )?;
        }
    }
    expect_eq(
        &pass.step()?,
        &Some(DiskANNMaintenanceStep {
            generation: live.generation(),
            status: DiskANNMaintenanceStatus::Retained,
        }),
        "live build is retained without blocking the discovery cursor",
    )?;
    expect_eq(
        &pass.step()?,
        &Some(DiskANNMaintenanceStep {
            generation: tail,
            status: DiskANNMaintenanceStatus::Reclaimed,
        }),
        "a retained generation does not starve a later abandoned build",
    )?;
    for _ in 0..2 {
        expect_eq(&pass.step()?, &None, "new builds cannot extend this pass")?;
    }
    for next in [earlier, later] {
        expect(
            repository.resume_stage(next, &control).is_ok(),
            "builds outside the discovery boundary survive its completion",
        )?;
    }
    drop(pass);
    expect_eq(
        &cleanup.memory().used(),
        &0,
        "closed pass releases its allowance",
    )?;
    store.vacuum()?;
    for reclaimed in [generation, earlier, later] {
        expect(
            store
                .scan_prefix_keys_after(Keys::new(reclaimed).prefix(), None, 1)?
                .is_empty(),
            "the next vacuum pass collects earlier and later abandoned generations",
        )?;
    }
    expect_eq(
        &held.get(payload_key.as_ref())?,
        &Some(vec![23; 32_768]),
        "retained payload remains readable after automatic deletion and vacuum",
    )?;
    expect_eq(
        &live.status(&control)?,
        &Some(crate::key_value::DiskANNStageStatus::Writing),
        "vacuum preserves the live build",
    )?;
    let live_generation = live.generation();
    drop((live, held));
    store.vacuum()?;
    expect(
        store
            .scan_prefix_keys_after(Keys::new(live_generation).prefix(), None, 1)?
            .is_empty(),
        "a subsequent pass observes last-owner release",
    )?;
    Ok(())
}

fn stage(
    store: &Arc<dyn KeyValueStore>,
    repository: &KeyValueDiskANNStore,
    table: u64,
    records: u64,
    control: &StorageReadControl,
) -> StorageBackendResult<KeyValueDiskANNStage> {
    let mut stage = repository.allocate_stage(table, 2, control)?;
    stage.start(control)?;
    let keys = Keys::new(stage.generation());
    let payload = vec![23; 32_768];
    store.with_mutation(&mut |_, batch| {
        batch.require_unchanged(keys.key(Kind::State).as_ref())?;
        for position in 0..records {
            batch.put(
                keys.key(Kind::Record(DiskANNRecordKey::Codes(position)))
                    .as_ref(),
                &payload,
            )?;
        }
        Ok(())
    })?;
    Ok(stage)
}
