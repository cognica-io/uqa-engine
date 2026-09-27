//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Population acceptance through the real canonical owner and persistent sessions.

use super::super::{
    identity::{row, Resolver},
    publication::{build, publish},
    setup, FIELD, TABLE,
};
use super::{canonical, diskann_runtime_fixture_options};
use crate::diskann_index::{
    format::DiskANNGeneration, DiskANNCanonicalCounts, DiskANNCanonicalRead, DiskANNPersistentOwner,
};
use crate::key_value::{
    conformance::{expect, expect_eq},
    vector_index::origin::populations::field_prefixes,
    KeyValueDiskANNHandle, KeyValueDiskANNStore,
};
use crate::{
    read_control::StorageReadControl, CatalogFacade, KeyValueCatalog, KeyValueStore,
    StorageBackendResult, VectorIndex,
};
use std::sync::Arc;

mod lifetimes;
mod rename;
pub use lifetimes::verify_diskann_population_lifetimes;

fn handle(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> StorageBackendResult<KeyValueDiskANNHandle> {
    canonical(store)?.bind(
        row([91; 16])?.relation,
        Arc::new(Resolver),
        diskann_runtime_fixture_options(2)?.read,
        control,
    )
}

fn counts(live: &KeyValueDiskANNHandle, current: u64, changed: u64) -> StorageBackendResult<()> {
    expect_eq(
        &live.query_metadata()?.canonical_counts,
        &Some(DiskANNCanonicalCounts::new(current, changed)?),
        "live fixed population header matches literal complete-origin membership",
    )
}

fn seed(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNGeneration> {
    let canonical = setup(store)?;
    KeyValueCatalog::new(store.clone()).save_catalog_index_row(&row([91; 16])?)?;
    canonical.replace(1, &[vec![1.0, 0.0]], control)?;
    canonical.replace(2, &[vec![0.0, 1.0], vec![1.0, 0.0]], control)?;
    canonical.replace(3, &[], control)?;
    rebuild(store, control)
}

fn rebuild(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNGeneration> {
    let repository = KeyValueDiskANNStore::connect(store, control)?;
    repository.initialize(control)?;
    let (coverage, stage) = build(&canonical(store)?, &repository, control)?;
    publish(store, &coverage, control)?;
    Ok(stage.generation())
}

/// Exercise both commit orders, command refresh and same-document conflicts through actual bound canonical replacements on a fresh disposable provider.
pub fn verify_diskann_population_writers(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 22);
    seed(store, &control)?;
    raw_writers(store, &control)?;
    let peer = store.open_session()?;
    let live = handle(store, &control)?;
    let other = handle(&peer, &control)?;
    for reverse in [false, true] {
        for refresh in [false, true] {
            live.replace(1, &[vec![1.0, 0.0]])?;
            live.replace(2, &[vec![0.0, 1.0], vec![1.0, 0.0]])?;
            rebuild(store, &control)?;
            counts(&live, 3, 0)?;
            store.begin_transaction()?;
            peer.begin_transaction()?;
            live.replace(1, &[vec![1.0, 0.0], vec![-1.0, 0.0]])?;
            other.replace(2, &[vec![0.0, 1.0]])?;
            counts(&live, 4, 2)?;
            counts(&other, 2, 1)?;
            let held = live.snapshot()?;
            let (first, second, second_handle) = if reverse {
                (&peer, store, &live)
            } else {
                (store, &peer, &other)
            };
            first.commit_transaction()?;
            if refresh {
                second.refresh_transaction_snapshot(control.cancellation())?;
                counts(second_handle, 3, 3)?;
            }
            second.commit_transaction()?;
            counts(&live, 3, 3)?;
            expect_eq(
                &held
                    .diskann_query_metadata(&control)?
                    .expect("retained metadata")
                    .canonical_counts,
                &Some(DiskANNCanonicalCounts::new(4, 2)?),
                "retained private population survives both writers and refresh",
            )?;
        }
    }
    store.begin_transaction()?;
    peer.begin_transaction()?;
    live.replace(1, &[vec![1.0, 0.0]])?;
    other.replace(1, &[])?;
    store.commit_transaction()?;
    expect(
        peer.commit_transaction().is_err(),
        "same-document origin conflict",
    )?;
    peer.rollback_transaction()?;
    counts(&other, 2, 2)
}

fn raw_writers(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    use crate::key_value::{KeyValueHNSWIndex, KeyValueIVFIndex, KeyValueVectorIndex};
    use crate::vector_index::{HNSWIndexParams, IVFIndexParams};
    let live = handle(store, control)?;
    for kind in 0..3 {
        let mut raw: Box<dyn VectorIndex> = match kind {
            0 => Box::new(KeyValueVectorIndex::new(store.clone(), TABLE, FIELD, 2)),
            1 => Box::new(KeyValueIVFIndex::create(
                store.clone(),
                TABLE,
                FIELD,
                2,
                IVFIndexParams::default(),
            )?),
            _ => Box::new(KeyValueHNSWIndex::create(
                store.clone(),
                TABLE,
                FIELD,
                2,
                HNSWIndexParams::default(),
            )?),
        };
        let before = store.scan_prefix(b"")?;
        for operation in 0..5 {
            let result = match operation {
                0 => raw.add(1, vec![0.0, 1.0]),
                1 => raw.add_many(1, vec![vec![0.0, 1.0], vec![1.0, 0.0]]),
                2 => raw.delete(2),
                3 => raw.delete(3),
                _ => raw.add(4, vec![1.0, 0.0]),
            };
            expect(
                result.is_err(),
                "raw vector owner must not invalidate a selected population",
            )?;
            expect_eq(
                &store.scan_prefix(b"")?,
                &before,
                "failed raw mutation preserves every record",
            )?;
            counts(&live, 3, 0)?;
            expect_eq(
                &raw.count()?,
                &3,
                "raw cache preserves its preceding generation",
            )?;
        }
    }
    store.begin_transaction()?;
    canonical(store)?.retire_index(&row([91; 16])?.relation, &Resolver, control)?;
    let mut raw = KeyValueVectorIndex::new(store.clone(), TABLE, FIELD, 2);
    raw.add(1, vec![0.0, 1.0])?;
    raw.delete(2)?;
    store.rollback_transaction()?;
    counts(&live, 3, 0)
}

/// A build captured before a concurrent replacement must census the final canonical view in either publication order, while earlier readers retain their own populations.
pub fn verify_diskann_population_late_publication(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 22);
    seed(store, &control)?;
    let peer = store.open_session()?;
    let live = handle(store, &control)?;
    let other = handle(&peer, &control)?;
    for writer_first in [false, true] {
        live.replace(1, &[vec![1.0, 0.0]])?;
        rebuild(store, &control)?;
        let held = live.snapshot()?;
        let repository = KeyValueDiskANNStore::connect(store, &control)?;
        let (coverage, _stage) = build(&canonical(store)?, &repository, &control)?;
        peer.begin_transaction()?;
        other.replace(1, &[vec![1.0, 0.0], vec![0.0, 1.0]])?;
        store.begin_transaction()?;
        publish(store, &coverage, &control)?;
        counts(&live, 3, 0)?;
        if writer_first {
            peer.commit_transaction()?;
            store.commit_transaction()?;
        } else {
            store.commit_transaction()?;
            peer.commit_transaction()?;
        }
        counts(&live, 4, 2)?;
        expect_eq(
            &held
                .diskann_query_metadata(&control)?
                .expect("retained metadata")
                .canonical_counts,
            &Some(DiskANNCanonicalCounts::new(3, 0)?),
            "replaced generation retains its original fixed header",
        )?;
    }
    Ok(())
}

/// Prepare a predecessor-shaped database whose selected generation and changed/empty canonical origins exist without derived population records. Close all owners before upgrading it.
pub fn seed_diskann_population_upgrade(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<DiskANNGeneration> {
    let control = StorageReadControl::with_limit(1 << 21);
    let generation = seed(store, &control)?;
    let live = handle(store, &control)?;
    live.replace(1, &[vec![1.0, 0.0]])?;
    live.replace(2, &[])?;
    counts(&live, 1, 1)?;
    store.with_mutation(&mut |_, batch| {
        for prefix in field_prefixes(TABLE, FIELD)? {
            batch.delete_prefix(&prefix)?;
        }
        Ok(())
    })?;
    Ok(generation)
}

/// Upgrade by the writable binding lifecycle, retaining the real build origins and caller undo. A concurrent first writer is included at final publication without rebuilding the graph.
pub fn verify_diskann_population_upgrade(
    store: &Arc<dyn KeyValueStore>,
    generation: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 22);
    let owner = canonical(store)?;
    let original = owner.retain(&control)?;
    expect_eq(
        &original.population_counts(generation, &control)?,
        &None,
        "legacy metadata remains bounded and absent",
    )?;
    let tiny = StorageReadControl::with_limit(1);
    expect(
        handle(store, &tiny).is_err(),
        "legacy binding obeys the original allowance",
    )?;
    store.begin_transaction()?;
    let private = handle(store, &control)?;
    counts(&private, 1, 1)?;
    store.rollback_transaction()?;
    expect_eq(
        &owner
            .retain(&control)?
            .population_counts(generation, &control)?,
        &None,
        "rollback undoes the entire initialization",
    )?;
    store.begin_transaction()?;
    let live = handle(store, &control)?;
    let held = live.snapshot()?;
    let peer = store.open_session()?;
    canonical(&peer)?.replace(3, &[vec![0.0, 1.0], vec![1.0, 0.0]], &control)?;
    store.commit_transaction()?;
    counts(&live, 3, 3)?;
    expect_eq(
        &live.query_metadata()?.manifest.input().generation,
        &generation,
        "legacy initialization preserves the physical generation",
    )?;
    expect_eq(
        &held
            .diskann_query_metadata(&control)?
            .expect("retained metadata")
            .canonical_counts,
        &Some(DiskANNCanonicalCounts::new(1, 1)?),
        "initialization preview remains retained after a late writer",
    )?;
    expect_eq(
        &original.population_counts(generation, &control)?,
        &None,
        "old read-only metadata is never silently rewritten",
    )?;
    Ok(())
}

/// Reopen the fully upgraded database after all initialization and late-writer owners have closed.
pub fn verify_diskann_population_upgrade_reopen(
    store: &Arc<dyn KeyValueStore>,
    generation: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 21);
    let live = handle(store, &control)?;
    counts(&live, 3, 3)?;
    expect_eq(
        &live.query_metadata()?.manifest.input().generation,
        &generation,
        "cold reopen retains the upgraded head",
    )?;
    live.replace(3, &[])?;
    counts(&live, 1, 1)
}
