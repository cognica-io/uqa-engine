//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{expect, expect_eq, fixture, write, Keys, Kind, MAX_RECORD};
use crate::key_value::{DiskANNStageStatus, KeyValueDiskANNStore};
use crate::{read_control::StorageReadControl, KeyValueStore, StorageBackendResult};
use std::sync::Arc;

/// Live writers, detached sealed sources and reopened owners exclude cleanup; only the last actual owner release admits bounded reclamation.
pub fn verify_diskann_build_ownership(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    repository.initialize(&control)?;
    let peer = KeyValueDiskANNStore::connect(store, &control)?;
    for frozen in [false, true] {
        let mut stage = repository.allocate_stage(71, 72, &control)?;
        stage.start(&control)?;
        let generation = stage.generation();
        let fixture = fixture(generation, &control)?;
        if frozen {
            expect(
                stage.seal(fixture.manifest, MAX_RECORD, &control).is_err(),
                "incomplete build stays frozen",
            )?;
        }
        expect(
            !peer.reclaim_abandoned_step(generation, 64, &control)?,
            "live staging owner excludes recovery",
        )?;
        expect(
            peer.resume_stage(generation, &control).is_err(),
            "another repository cannot adopt a live writer",
        )?;
        drop(stage);
        let resumed = peer.resume_stage(generation, &control)?;
        expect(
            !repository.reclaim_abandoned_step(generation, 64, &control)?,
            "resumed owner also excludes recovery",
        )?;
        drop(resumed);
        expect(
            repository.reclaim_abandoned_step(generation, 64, &control)?,
            "last writer release admits cleanup",
        )?;
    }

    let mut stage = repository.allocate_stage(71, 72, &control)?;
    stage.start(&control)?;
    let generation = stage.generation();
    let fixture = fixture(generation, &control)?;
    write(&stage, &fixture, &control)?;
    drop(stage.seal(fixture.manifest, MAX_RECORD, &control)?);
    let source = peer.open_source(generation, &control)?;
    let held = source.read.retain(&[super::super::keys::ROOT])?;
    drop((source, stage));
    expect(
        !repository.reclaim_abandoned_step(generation, 64, &control)?,
        "nested retained source owns the physical build",
    )?;
    let page = Keys::new(generation).key(Kind::Graph(0));
    expect_eq(
        &held.get(page.as_ref())?.as_deref(),
        &Some(fixture.page.as_slice()),
        "retained graph bytes remain intact",
    )?;
    drop(held);
    expect(
        !repository.reclaim_abandoned_step(generation, 1, &control)?,
        "abandoned seal cleanup remains bounded",
    )?;
    expect_eq(
        &peer.resume_stage(generation, &control)?.status(&control)?,
        &Some(DiskANNStageStatus::Discarding),
        "partial abandoned cleanup is durable",
    )?;
    expect(
        peer.reclaim_abandoned_step(generation, 64, &control)?,
        "new cleanup owner completes prior work",
    )?;
    expect(
        peer.reclaim_abandoned_step(generation, 64, &control)?,
        "completed cleanup is idempotent",
    )?;

    verify_legacy_ownership(store, &repository, &peer, &control)
}

fn verify_legacy_ownership(
    store: &Arc<dyn KeyValueStore>,
    repository: &KeyValueDiskANNStore,
    peer: &KeyValueDiskANNStore,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    for resume in [false, true] {
        let mut stage = repository.allocate_stage(71, 72, control)?;
        stage.start(control)?;
        let generation = stage.generation();
        drop(stage);
        let state_key = Keys::new(generation).key(Kind::State);
        let legacy = super::super::state::State {
            status: DiskANNStageStatus::Writing,
            owner: super::super::state::StageOwner::Legacy([9; 16]),
        }
        .encode();
        store.with_mutation(&mut |_, batch| batch.put(state_key.as_ref(), &legacy))?;
        if resume {
            let stage = repository.resume_stage(generation, control)?;
            expect_eq(
                &store.get(state_key.as_ref())?.unwrap()[0],
                &2,
                "legacy ownership upgrades before writes",
            )?;
            expect(
                !peer.reclaim_abandoned_step(generation, 64, control)?,
                "upgraded live owner excludes recovery",
            )?;
            drop(stage);
        }
        expect(
            peer.reclaim_abandoned_step(generation, 64, control)?,
            "fenced legacy owners can resume or retire without payload conversion",
        )?;
    }
    Ok(())
}
