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
    changes::{DiskANNPruneCursor, DiskANNPruneRequest, DiskANNPruneResult},
    format::{DiskANNChangeIdentity, DiskANNGeneration},
};
use crate::key_value::{
    conformance::{expect, expect_eq},
    vector_index::origin::journal,
    KeyValueDiskANNCanonical, KeyValueDiskANNPruner, KeyValueDiskANNStore,
    RetainedDiskANNCanonical,
};
use crate::{
    read_control::StorageReadControl, CatalogFacade, KeyValueCatalog, KeyValueStore,
    StorageBackendResult,
};
use std::sync::Arc;

fn prune(
    store: &Arc<dyn KeyValueStore>,
    source: &RetainedDiskANNCanonical,
    pruner: &KeyValueDiskANNPruner,
    request: DiskANNPruneRequest,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNPruneResult> {
    let mut result = None;
    store.with_mutation(&mut |read, batch| {
        result = Some(source.prune_changes(&Resolver, pruner, (read, batch), request, control)?);
        Ok(())
    })?;
    result.ok_or_else(|| crate::StorageBackendError::Other("pruning callback did not run".into()))
}

fn page(max_records: usize) -> DiskANNPruneRequest {
    DiskANNPruneRequest {
        after: None,
        max_records,
    }
}

/// Actual bounded cleanup with late writers, obsolete/empty versions, savepoint undo, retained readers and head/catalog races. Supply a fresh disposable versioned database and close every owner before the reopen check.
pub fn verify_diskann_pruning(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<DiskANNGeneration> {
    let control = StorageReadControl::with_limit(1 << 20);
    let canonical = setup(store)?;
    KeyValueCatalog::new(store.clone()).save_catalog_index_row(&row([91; 16])?)?;
    let original = canonical.replace(1, &[vec![1.0, 0.0]], &control)?;
    canonical.replace(2, &[], &control)?;
    canonical.replace(3, &[], &control)?;
    canonical.replace(3, &[vec![0.0, 1.0]], &control)?;
    let peer = store.open_session()?;
    let peer_canonical = KeyValueDiskANNCanonical::new(peer.clone(), TABLE, FIELD, 2)?;
    peer.begin_transaction()?;
    let late = peer_canonical.replace(4, &[], &control)?;
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    repository.initialize(&control)?;
    let (coverage, stage) = build(&canonical, &repository, &control)?;
    publish(store, &coverage, &control)?;
    let pruner = KeyValueDiskANNPruner::open(
        repository.open_source(stage.generation(), &control)?,
        8192,
        &control,
    )?;
    let source = coverage.source();
    let changed = canonical.replace(1, &[vec![0.0, 1.0]], &control)?;
    peer.commit_transaction()?;
    let old_cursor = prune_pass(store, &canonical, source, &pruner, &control)?;
    expect_eq(
        &source.origin(1, &control)?,
        &Some(original),
        "old canonical origin survives cleanup",
    )?;
    expect_eq(
        &source
            .next_change_after(None, &control)?
            .map(DiskANNChangeIdentity::document),
        &Some(1),
        "old reader retains deleted journal entry",
    )?;
    let current = canonical.retain(&control)?;
    expect_eq(
        &current
            .next_change_after(None, &control)?
            .map(DiskANNChangeIdentity::version),
        &Some(changed),
        "current changed tensor remains journaled",
    )?;
    expect_eq(
        &current
            .next_change_after(Some(1), &control)?
            .map(DiskANNChangeIdentity::version),
        &Some(late),
        "earlier allocated later commit remains journaled",
    )?;
    publication_races(
        store,
        &canonical,
        source,
        &repository,
        &pruner,
        old_cursor,
        &control,
    )
}

fn prune_pass(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    source: &RetainedDiskANNCanonical,
    pruner: &KeyValueDiskANNPruner,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<DiskANNPruneCursor>> {
    let prefix = journal::prefix(TABLE, FIELD)?;
    expect_eq(
        &store.scan_prefix(&prefix)?.len(),
        &6,
        "six actual journal mutations",
    )?;
    store.begin_transaction()?;
    store.savepoint("pruning")?;
    expect_eq(
        &prune(store, source, pruner, page(64), control)?.removed,
        &4,
        "covered and obsolete versions staged",
    )?;
    store.rollback_to_savepoint("pruning")?;
    expect_eq(
        &store.scan_prefix(&prefix)?.len(),
        &6,
        "pruning undo restores every journal entry",
    )?;
    canonical.replace(1, &[], control)?;
    expect(
        prune(store, source, pruner, page(64), control).is_err(),
        "private input cannot authorize pruning",
    )?;
    store.rollback_transaction()?;
    expect(
        prune(store, source, pruner, page(0), control).is_err(),
        "zero page rejected",
    )?;
    expect(
        prune(
            store,
            source,
            pruner,
            page(64),
            &StorageReadControl::with_limit(1),
        )
        .is_err(),
        "quota rejects before deletion",
    )?;
    let mut request = page(1);
    let mut examined = 0;
    let mut removed = 0;
    let old_cursor;
    loop {
        let result = prune(store, source, pruner, request, control)?;
        expect(
            result.examined <= 1 && result.removed <= 1,
            "bounded exact-key page",
        )?;
        examined += result.examined;
        removed += result.removed;
        if result.next.is_none() {
            old_cursor = request.after;
            break;
        }
        request.after = result.next;
    }
    expect_eq(
        &(examined, removed),
        &(6, 4),
        "one pass covers all versions and preserves late changes",
    )?;
    Ok(old_cursor)
}

fn publication_races(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    source: &RetainedDiskANNCanonical,
    repository: &KeyValueDiskANNStore,
    pruner: &KeyValueDiskANNPruner,
    old_cursor: Option<DiskANNPruneCursor>,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNGeneration> {
    let peer = store.open_session()?;
    let prefix = journal::prefix(TABLE, FIELD)?;
    let obsolete = canonical.replace(0, &[], control)?;
    canonical.replace(0, &[], control)?;
    expect_eq(
        &prune(
            store,
            source,
            pruner,
            DiskANNPruneRequest {
                after: old_cursor,
                max_records: 64,
            },
            control,
        )?
        .examined,
        &0,
        "new earlier keys do not redefine a completed cursor",
    )?;
    let (new_coverage, new_stage) = build(canonical, repository, control)?;
    store.begin_transaction()?;
    expect_eq(
        &prune(store, source, pruner, page(64), control)?.removed,
        &1,
        "fresh pass discovers earlier obsolete key",
    )?;
    publish(&peer, &new_coverage, control)?;
    expect(
        store.commit_transaction().is_err(),
        "head switch rejects evaluated cleanup",
    )?;
    store.rollback_transaction()?;
    expect(
        store
            .get(&journal::key(
                TABLE,
                FIELD,
                DiskANNChangeIdentity::new(0, obsolete),
            )?)?
            .is_some(),
        "failed cleanup publishes no deletion",
    )?;
    expect(
        prune(store, source, pruner, page(64), control).is_err(),
        "retired coverage cannot prune new head",
    )?;
    let pruner = KeyValueDiskANNPruner::open(
        repository.open_source(new_stage.generation(), control)?,
        8192,
        control,
    )?;
    expect(
        prune(
            store,
            source,
            &pruner,
            DiskANNPruneRequest {
                after: old_cursor,
                max_records: 64,
            },
            control,
        )
        .is_err(),
        "cursor belongs to its original generation",
    )?;
    store.begin_transaction()?;
    prune(store, source, &pruner, page(64), control)?;
    KeyValueCatalog::new(peer).save_catalog_index_row(&row([91; 16])?)?;
    expect(
        store.commit_transaction().is_err(),
        "catalog change rejects evaluated cleanup",
    )?;
    store.rollback_transaction()?;
    let fresh = canonical.retain_for_index(&row([91; 16])?.relation, control)?;
    malformed_and_controls(store, &fresh, &pruner, control)?;
    canonical.replace(9, &[], control)?;
    expect_eq(
        &prune(store, &fresh, &pruner, page(64), control)?.removed,
        &4,
        "new published coverage retires exact current and obsolete records",
    )?;
    expect_eq(
        &store.scan_prefix(&prefix)?.len(),
        &1,
        "only post-build mutation remains",
    )?;
    Ok(new_stage.generation())
}

fn malformed_and_controls(
    store: &Arc<dyn KeyValueStore>,
    source: &RetainedDiskANNCanonical,
    pruner: &KeyValueDiskANNPruner,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let keys = store.scan_prefix(&journal::prefix(TABLE, FIELD)?)?;
    let (key, value) = keys.last().expect("fixture has late changes");
    store.put(key, b"bad journal origin")?;
    expect(
        prune(store, source, pruner, page(64), control).is_err(),
        "corrupt late record fails whole pruning batch",
    )?;
    expect_eq(
        &store.scan_prefix(&journal::prefix(TABLE, FIELD)?)?.len(),
        &keys.len(),
        "earlier evaluated deletes roll back on decoding failure",
    )?;
    store.put(key, value)?;
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    expect(
        prune(store, source, pruner, page(64), &cancelled).is_err(),
        "cancelled pruning preserves journal",
    )
}

/// Verify selected coverage and the sole uncovered mutation after every provider owner has closed.
pub fn verify_diskann_pruning_reopen(
    store: &Arc<dyn KeyValueStore>,
    generation: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let canonical = KeyValueDiskANNCanonical::new(store.clone(), TABLE, FIELD, 2)?;
    let source = canonical.retain_for_index(&row([91; 16])?.relation, &control)?;
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    let pruner = KeyValueDiskANNPruner::open(
        repository.open_source(generation, &control)?,
        8192,
        &control,
    )?;
    let result = prune(store, &source, &pruner, page(64), &control)?;
    expect_eq(
        &(result.examined, result.removed, result.next),
        &(1, 0, None),
        "cold reopen preserves uncovered journal",
    )?;
    expect_eq(
        &source
            .next_change_after(None, &control)?
            .map(DiskANNChangeIdentity::document),
        &Some(9),
        "reopened changed-document query",
    )?;
    expect_eq(
        &store.scan_prefix(&journal::prefix(TABLE, FIELD)?)?.len(),
        &1,
        "cold reopen retains bounded physical deletion",
    )
}
