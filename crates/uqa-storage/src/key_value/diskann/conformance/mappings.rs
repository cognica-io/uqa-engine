//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use crate::diskann_index::catalog::{resolve_scope, DiskANNIndexResolver, DiskANNIndexScope};
use crate::key_value::{
    DiskANNStageStatus, KeyValueDiskANNMaintenance, KeyValueDiskANNMappingMaintenance,
    KeyValueDiskANNStore,
};
use crate::{read_control::StorageReadControl, KeyValueStore, StorageBackendResult};

use super::super::keys::ROOT;
use super::{expect, expect_eq};

struct Resolver([u8; 16]);
impl DiskANNIndexResolver for Resolver {
    fn resolve(
        &self,
        definition: &str,
        table: [u8; 16],
        _: &StorageReadControl,
    ) -> StorageBackendResult<[u8; 16]> {
        expect_eq(&definition, &"mapping fixture", "captured definition")?;
        expect_eq(&table, &[211; 16], "complete catalog table identity")?;
        Ok(self.0)
    }
}

fn scope(
    store: &Arc<dyn KeyValueStore>,
    index: u8,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNIndexScope> {
    let mut selected = None;
    store.with_read_view(&mut |read| {
        let revision = read
            .record_revision(b"diskann-mapping-fixture")?
            .expect("fixture record");
        selected = Some(resolve_scope(
            &Resolver([index; 16]),
            ([211; 16], [212; 16]),
            Some("mapping fixture"),
            &revision,
            read.control(),
            control,
            control,
        )?);
        Ok(())
    })?;
    Ok(selected.expect("scope completion"))
}

fn keys(database: [u8; 16], index: u8) -> [Vec<u8>; 4] {
    [2, 3, 6, 7].map(|tag| {
        let mut key = ROOT.to_vec();
        key.push(tag);
        key.extend_from_slice(&database);
        key.extend_from_slice(&[211; 16]);
        key.extend_from_slice(&[212; 16]);
        if tag == 3 || tag == 7 {
            key.extend_from_slice(&[index; 16]);
        }
        key
    })
}

pub(super) fn verify(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    store.put(b"diskann-mapping-fixture", b"mapping fixture")?;
    let scope = scope(store, 213, control)?;
    let repository = KeyValueDiskANNStore::connect(store, control)?;
    let mut stage = repository.allocate_bound_stage(&scope, control)?;
    let generation = stage.generation();
    let keys = keys(generation.database(), 213);
    expect_eq(
        &stage.status(control)?,
        &Some(DiskANNStageStatus::Writing),
        "bound reservation already owns its first state",
    )?;
    let mut retained = None;
    store.with_read_view(&mut |read| {
        retained = Some(read.retain(&[ROOT])?);
        Ok(())
    })?;
    let retained = retained.expect("mapping snapshot");
    let original = [store.get(&keys[0])?, store.get(&keys[1])?];
    for (position, handle) in [generation.table(), generation.index()]
        .into_iter()
        .enumerate()
    {
        let mut bytes = vec![1];
        bytes.extend_from_slice(&handle.to_be_bytes());
        expect_eq(
            &original[position],
            &Some(bytes),
            "mapping bytes remain the stable nine-byte handle",
        )?;
    }
    for key in &keys[2..] {
        expect_eq(
            &store.get(key)?.as_deref(),
            &Some(generation.generation().to_be_bytes().as_slice()),
            "preparation guard records the reserved generation",
        )?;
    }
    KeyValueDiskANNMaintenance::run(store, control)?;
    expect_eq(
        &store.get(&keys[1])?,
        &original[1],
        "live bound reservation excludes reclamation before explicit start",
    )?;
    stage.start(control)?;
    drop(stage);
    KeyValueDiskANNMaintenance::run(store, control)?;
    for key in &keys {
        expect_eq(
            &store.get(key)?,
            &None,
            "last abandoned generation releases its mapping and guard",
        )?;
    }
    for position in 0..2 {
        expect_eq(
            &retained.get(&keys[position])?.as_deref(),
            &original[position].as_deref(),
            "older MVCC view retains original mapping bytes",
        )?;
    }
    let mut allocator = ROOT.to_vec();
    allocator.push(4);
    allocator.extend_from_slice(&generation.database());
    expect_eq(
        &store
            .identifier_allocator()
            .expect("identifiers")
            .identifier_watermark(&allocator)?,
        &Some(generation.index()),
        "physical handle watermark survives mapping retirement",
    )?;
    let stage = repository.allocate_bound_stage(&scope, control)?;
    let replacement = stage.generation();
    expect(
        replacement.table() > generation.table() && replacement.index() > generation.index(),
        "recreated mappings never reuse retired physical handles",
    )?;
    finite_pass(store, &repository, stage, control)?;
    drop(retained);
    store.delete(b"diskann-mapping-fixture")
}

fn finite_pass(
    store: &Arc<dyn KeyValueStore>,
    repository: &KeyValueDiskANNStore,
    stage: super::super::KeyValueDiskANNStage,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let first = stage.generation();
    let mut pass = KeyValueDiskANNMappingMaintenance::start(store, control)?;
    let earlier = repository.allocate_bound_stage(&scope(store, 1, control)?, control)?;
    let later = repository.allocate_bound_stage(&scope(store, 254, control)?, control)?;
    expect_eq(
        &earlier.generation().table(),
        &first.table(),
        "new index shares the actual table mapping",
    )?;
    drop(stage);
    expect(
        repository.reclaim_abandoned_step(first, 64, control)?,
        "unowned first generation is reclaimable",
    )?;
    while pass.step()?.is_some() {}
    expect_eq(
        &store.get(&keys(first.database(), 213)[1])?,
        &None,
        "captured obsolete index mapping is deleted",
    )?;
    for index in [1, 254] {
        expect(
            store.get(&keys(first.database(), index)[1])?.is_some(),
            "new mappings outside the discovery snapshot survive",
        )?;
    }
    expect(
        store.get(&keys(first.database(), 213)[0])?.is_some(),
        "table mapping stays while new child indexes remain",
    )?;
    drop((earlier, later));
    KeyValueDiskANNMaintenance::run(store, control)?;
    for index in [1, 213, 254] {
        for key in keys(first.database(), index) {
            expect_eq(
                &store.get(&key)?,
                &None,
                "next finite pass removes obsolete maps and guards",
            )?;
        }
    }
    Ok(())
}
