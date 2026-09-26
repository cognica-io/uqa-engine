//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    identity::{row, Resolver},
    publication::{build, publish},
    setup, FIELD, TABLE,
};
use crate::diskann_index::{
    format::{DiskANNGeneration, PAGE_BYTES},
    pages::DiskANNReadLimits,
};
use crate::key_value::{
    conformance::{expect, expect_eq},
    KeyValueDiskANNCanonical, KeyValueDiskANNHandle, KeyValueDiskANNStore,
};
use crate::{
    read_control::StorageReadControl, CatalogFacade, KeyValueCatalog, KeyValueStore,
    StorageBackendResult, VectorIndex,
};
use std::sync::Arc;

fn limits() -> DiskANNReadLimits {
    DiskANNReadLimits {
        resident_bytes: 65_536,
        cache_bytes: PAGE_BYTES,
        max_in_flight_page_bytes: 2 * PAGE_BYTES,
        max_record_bytes: 8192,
    }
}

fn handle(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> StorageBackendResult<KeyValueDiskANNHandle> {
    KeyValueDiskANNCanonical::new(store.clone(), TABLE, FIELD, 2)?.bind(
        row([91; 16])?.relation,
        Arc::new(Resolver),
        limits(),
        control,
    )
}

fn scores(index: &dyn VectorIndex, expected: &[(u64, f32)]) -> StorageBackendResult<()> {
    let postings = index.search_knn(&[1.0, 0.0], 10)?;
    expect_eq(
        &postings
            .iter()
            .map(|entry| (entry.doc_id, entry.payload.score.to_bits()))
            .collect::<Vec<_>>(),
        &expected
            .iter()
            .map(|&(document, score)| (document, f64::from(score).to_bits()))
            .collect::<Vec<_>>(),
        "live search keeps literal complete-tensor cosine scores",
    )
}

/// Exercise bound live writes and real paged searches, private undo, independent writers, conflicts, actual generation replacement and definition promotion on a disposable provider. Release every owner before checking the returned generation through cold reopen.
pub fn verify_diskann_live_writes(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<DiskANNGeneration> {
    let control = StorageReadControl::with_limit(1 << 21);
    let canonical = setup(store)?;
    let catalog = KeyValueCatalog::new(store.clone());
    catalog.save_catalog_index_row(&row([91; 16])?)?;
    canonical.replace(1, &[vec![1.0, 0.0]], &control)?;
    canonical.replace(2, &[], &control)?;
    expect(
        handle(store, &control).is_err(),
        "missing publication cannot open live",
    )?;
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    repository.initialize(&control)?;
    let (coverage, stage) = build(&canonical, &repository, &control)?;
    publish(store, &coverage, &control)?;
    drop((coverage, stage));
    let live = handle(store, &control)?;
    let held = live.snapshot()?;
    scores(&held, &[(1, 1.0)])?;

    store.begin_transaction()?;
    store.savepoint("live-write")?;
    let undone = live.replace(1, &[vec![-1.0, 0.0]])?;
    live.replace(2, &[vec![0.0, 1.0], vec![1.0, 0.0]])?;
    let private = live.snapshot()?;
    scores(&private, &[(1, -1.0), (2, 1.0)])?;
    expect_eq(
        &private.count()?,
        &3,
        "tensor ordinal count after replacement",
    )?;
    expect(
        live.replace(1, &[vec![1.0]]).is_err(),
        "invalid dimensions fail atomically",
    )?;
    expect_eq(
        &canonical.retain(&control)?.origin(1, &control)?,
        &Some(undone),
        "failed write preserves origin",
    )?;
    store.rollback_to_savepoint("live-write")?;
    scores(&live.snapshot()?, &[(1, 1.0)])?;
    scores(&private, &[(1, -1.0), (2, 1.0)])?;
    let replacement = live.replace(1, &[vec![0.0, 1.0]])?;
    expect(
        undone != replacement,
        "undo never reuses a mutation identity",
    )?;
    store.commit_transaction()?;
    scores(&live.snapshot()?, &[(1, 0.0)])?;
    scores(&held, &[(1, 1.0)])?;

    let peer = concurrent_writers(store, &live, &control)?;

    // A real private definition and generation become committed; the live identity survives that promotion.
    store.begin_transaction()?;
    catalog.save_catalog_index_row(&row([91; 16])?)?;
    let (coverage, stage) = build(&canonical, &repository, &control)?;
    publish(store, &coverage, &control)?;
    let promoted = handle(store, &control)?;
    let generation = stage.generation();
    store.commit_transaction()?;
    promoted.replace(5, &[vec![0.0, 1.0]])?;
    expect_eq(
        &promoted.snapshot()?.manifest().input().generation,
        &generation,
        "live snapshot selects the current actual head",
    )?;
    scores(&held, &[(1, 1.0)])?;
    scores(&private, &[(1, -1.0), (2, 1.0)])?;
    drop((coverage, stage));

    admission_failures(store, &live, &control)?;
    definition_guards(store, &peer, &live, &control)?;
    let tiny = StorageReadControl::with_limit(1);
    expect(
        handle(store, &tiny).is_err(),
        "opening a live handle preserves its allowance",
    )?;
    expect_eq(
        &tiny.memory().used(),
        &0,
        "failed handle releases admission",
    )?;
    scores(
        &live.snapshot()?,
        &[(1, 1.0), (3, 1.0), (4, -1.0), (5, 0.0)],
    )?;
    control.cancellation().cancel();
    expect(
        live.replace(1, &[]).is_err(),
        "original cancellation rejects writes",
    )?;
    expect(
        live.snapshot().is_err(),
        "original cancellation rejects fresh reads",
    )?;
    Ok(generation)
}

fn concurrent_writers(
    store: &Arc<dyn KeyValueStore>,
    live: &KeyValueDiskANNHandle,
    control: &StorageReadControl,
) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
    let peer = store.open_session()?;
    let other = handle(&peer, control)?;
    store.begin_transaction()?;
    peer.begin_transaction()?;
    live.replace(3, &[vec![1.0, 0.0]])?;
    other.replace(4, &[vec![-1.0, 0.0]])?;
    store.commit_transaction()?;
    peer.commit_transaction()?;
    scores(&live.snapshot()?, &[(1, 0.0), (3, 1.0), (4, -1.0)])?;

    store.begin_transaction()?;
    peer.begin_transaction()?;
    live.replace(1, &[vec![1.0, 0.0]])?;
    other.replace(1, &[vec![-1.0, 0.0]])?;
    store.commit_transaction()?;
    expect(
        peer.commit_transaction().is_err(),
        "same-document replacement conflicts",
    )?;
    peer.rollback_transaction()?;
    scores(&other.snapshot()?, &[(1, 1.0), (3, 1.0), (4, -1.0)])?;

    Ok(peer)
}

fn admission_failures(
    store: &Arc<dyn KeyValueStore>,
    live: &KeyValueDiskANNHandle,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let canonical = KeyValueDiskANNCanonical::new(store.clone(), TABLE, FIELD, 2)?;
    let reserved = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())?;
    expect(
        live.replace(9, &[vec![1.0, 0.0]]).is_err(),
        "mutation cannot bypass the original exhausted allowance",
    )?;
    expect(
        live.snapshot().is_err(),
        "fresh preparation cannot bypass the original allowance",
    )?;
    drop(reserved);
    expect(
        canonical.retain(control)?.origin(9, control)?.is_none(),
        "failed admission leaves no canonical origin",
    )?;
    Ok(())
}

fn definition_guards(
    store: &Arc<dyn KeyValueStore>,
    peer: &Arc<dyn KeyValueStore>,
    live: &KeyValueDiskANNHandle,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(store.clone());
    store.begin_transaction()?;
    live.replace(9, &[vec![1.0, 0.0]])?;
    KeyValueCatalog::new(peer.clone()).save_catalog_index_row(&row([91; 16])?)?;
    expect(
        store.commit_transaction().is_err(),
        "definition change after evaluation rejects commit",
    )?;
    store.rollback_transaction()?;
    expect(
        !live.snapshot()?.contains_document(9)?,
        "failed catalog race preserves all canonical effects",
    )?;

    for kind in 0..3 {
        store.begin_transaction()?;
        match kind {
            0 => {
                catalog.drop_catalog_index(&row([91; 16])?.relation)?;
                catalog.save_catalog_index_row(&row([92; 16])?)?;
            }
            1 => {
                let mut changed = row([91; 16])?;
                let mut params = crate::vector_index::DiskANNIndexParams::for_dimensions(2)?;
                params.search_list_size += 1;
                changed.parameters_json = serde_json::to_string(&params.to_catalog_map(2)?)?;
                catalog.save_catalog_index_row(&changed)?;
            }
            _ => {
                let mut table = catalog
                    .load_tables()?
                    .into_iter()
                    .find(|table| table.relation.qualified_name() == TABLE)
                    .ok_or_else(|| {
                        crate::StorageBackendError::Other(
                            "KeyValue conformance failed: missing live DiskANN fixture table"
                                .into(),
                        )
                    })?;
                table.storage_generation = [81; 16];
                catalog.save_table(&table)?;
            }
        }
        expect(
            live.replace(9, &[vec![1.0, 0.0]]).is_err(),
            "stale incarnation/configuration cannot write",
        )?;
        expect(
            live.snapshot().is_err(),
            "stale handle cannot silently rebind queries",
        )?;
        store.rollback_transaction()?;
        let canonical = KeyValueDiskANNCanonical::new(store.clone(), TABLE, FIELD, 2)?;
        expect(
            canonical.retain(control)?.origin(9, control)?.is_none(),
            "rejected stale write leaves no origin",
        )?;
    }
    Ok(())
}

/// Verify complete cold reopen and another live write on the original published generation. This must run after releasing the previous provider and every retained reader.
pub fn verify_diskann_live_reopen(
    store: &Arc<dyn KeyValueStore>,
    generation: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let live = handle(store, &control)?;
    let before = live.snapshot()?;
    expect_eq(
        &before.manifest().input().generation,
        &generation,
        "reopened live physical generation",
    )?;
    scores(&before, &[(1, 1.0), (3, 1.0), (4, -1.0), (5, 0.0)])?;
    live.replace(1, &[])?;
    live.replace(6, &[vec![1.0, 0.0]])?;
    let after = live.snapshot()?;
    scores(&after, &[(3, 1.0), (4, -1.0), (5, 0.0), (6, 1.0)])?;
    expect(
        !after.contains_document(1)?,
        "empty replacement removes vector membership",
    )?;
    expect_eq(&after.count()?, &4, "reopened tensor count")?;
    scores(&before, &[(1, 1.0), (3, 1.0), (4, -1.0), (5, 0.0)])
}
