//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    identity::{row, Resolver},
    setup, FIELD, TABLE,
};
use crate::diskann_index::{
    build::DiskANNCanonicalCoverage, catalog::DiskANNIndexScope, format::DiskANNGeneration,
    pages::DiskANNOriginReader,
};
use crate::key_value::{
    conformance::{build_diskann_publication_fixture, expect, expect_eq},
    publication::selected_generation,
    DiskANNStageStatus, KeyValueDiskANNCanonical, KeyValueDiskANNStage, KeyValueDiskANNStore,
    RetainedDiskANNCanonical,
};
use crate::{
    read_control::StorageReadControl, CatalogFacade, KeyValueCatalog, KeyValueStore,
    StorageBackendResult,
};
use std::sync::Arc;

type Coverage = DiskANNCanonicalCoverage<RetainedDiskANNCanonical>;

pub(super) fn build(
    canonical: &KeyValueDiskANNCanonical,
    repository: &KeyValueDiskANNStore,
    control: &StorageReadControl,
) -> StorageBackendResult<(Coverage, KeyValueDiskANNStage)> {
    let source = canonical.retain_for_index(&row([91; 16])?.relation, control)?;
    let scope = source.index_scope(&Resolver, control)?;
    let parameters = source.index_parameters().expect("bound source");
    let mut stage = repository.allocate_bound_stage(&scope, control)?;
    let coverage = build_diskann_publication_fixture(source, &mut stage, parameters, control)?;
    Ok((coverage, stage))
}

pub(super) fn publish(
    store: &Arc<dyn KeyValueStore>,
    coverage: &Coverage,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let repository = KeyValueDiskANNStore::connect(store, control)?;
    let sealed = repository.open_source(coverage.fingerprint().generation(), control)?;
    store.with_mutation(&mut |read, batch| {
        RetainedDiskANNCanonical::publish_generation(
            coverage, &Resolver, &sealed, read, batch, control,
        )
    })
}

fn selected(
    store: &Arc<dyn KeyValueStore>,
    scope: &DiskANNIndexScope,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<DiskANNGeneration>> {
    let mut result = None;
    store.with_read_view(&mut |read| {
        result = selected_generation(scope, read, control)?;
        Ok(())
    })?;
    Ok(result)
}

/// Exercise actual head/coverage transactions, stale and competing builds, private rollback, definition conflicts and retained views. Use a fresh disposable versioned database; reopen the returned generation after releasing every owner.
pub fn verify_diskann_publication(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<DiskANNGeneration> {
    let control = StorageReadControl::with_limit(1 << 20);
    let canonical = setup(store)?;
    let catalog = KeyValueCatalog::new(store.clone());
    let definition = row([91; 16])?;
    catalog.save_catalog_index_row(&definition)?;
    let original = canonical.replace(1, &[vec![1.0, 0.0]], &control)?;
    canonical.replace(2, &[], &control)?;
    let source = canonical.retain_for_index(&definition.relation, &control)?;
    let scope = source.index_scope(&Resolver, &control)?;
    let parameters = source.index_parameters().unwrap();
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    repository.initialize(&control)?;
    let mut first_stage = repository.allocate_bound_stage(&scope, &control)?;
    let first = build_diskann_publication_fixture(source, &mut first_stage, parameters, &control)?;
    let (stale, mut stale_stage) = build(&canonical, &repository, &control)?;
    let peer = store.open_session()?;
    let peer_canonical = KeyValueDiskANNCanonical::new(peer.clone(), TABLE, FIELD, 2)?;
    let late = peer_canonical.replace(1, &[vec![0.0, 1.0]], &control)?;
    store.begin_transaction()?;
    publish(store, &first, &control)?;
    expect_eq(
        &selected(store, &scope, &control)?,
        &Some(first_stage.generation()),
        "private head is selected atomically",
    )?;
    expect_eq(
        &selected(&peer, &scope, &control)?,
        &None,
        "private publication is invisible to peer",
    )?;
    store.rollback_transaction()?;
    expect_eq(&selected(store, &scope, &control)?, &None, "head rollback")?;
    expect_eq(
        &first_stage.status(&control)?,
        &Some(DiskANNStageStatus::Sealed),
        "rollback preserves physical seal",
    )?;
    publish(store, &first, &control)?;
    expect_eq(
        &first_stage.status(&control)?,
        &Some(DiskANNStageStatus::Published),
        "committed published state",
    )?;
    expect(
        first_stage.discard_step(64, &control).is_err(),
        "staging cannot delete a published generation",
    )?;
    let reader = DiskANNOriginReader::open(
        repository.open_source(first_stage.generation(), &control)?,
        8192,
        &control,
    )?;
    expect_eq(
        &reader.origin(1, &control)?.unwrap().version(),
        &original,
        "published origins retain captured tensor",
    )?;
    expect_eq(
        &peer_canonical.retain(&control)?.origin(1, &control)?,
        &Some(late),
        "later canonical commit survives publication",
    )?;
    expect(
        publish(store, &stale, &control).is_err(),
        "old missing head cannot replace newer generation",
    )?;
    expect(
        stale_stage.discard_step(64, &control).is_err(),
        "sealed stale generation requires lifecycle reclamation",
    )?;
    competing(
        store,
        &canonical,
        &repository,
        &scope,
        &mut first_stage,
        &control,
    )?;
    private_and_definition(store, &canonical, &repository, &scope, &control)
}

fn competing(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    repository: &KeyValueDiskANNStore,
    scope: &DiskANNIndexScope,
    first: &mut KeyValueDiskANNStage,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let peer = store.open_session()?;
    let peer_canonical = KeyValueDiskANNCanonical::new(peer.clone(), TABLE, FIELD, 2)?;
    let mut retained = None;
    store.with_read_view(&mut |read| {
        retained = Some(read.retain(&[b""])?);
        Ok(())
    })?;
    let (winner, winner_stage) = build(canonical, repository, control)?;
    let (loser, loser_stage) = build(&peer_canonical, repository, control)?;
    store.begin_transaction()?;
    peer.begin_transaction()?;
    publish(store, &winner, control)?;
    publish(&peer, &loser, control)?;
    store.commit_transaction()?;
    expect(
        peer.commit_transaction().is_err(),
        "competing publication loses its original head condition",
    )?;
    peer.rollback_transaction()?;
    expect_eq(
        &selected(store, scope, control)?,
        &Some(winner_stage.generation()),
        "one winning head",
    )?;
    expect_eq(
        &first.status(control)?,
        &Some(DiskANNStageStatus::Retired),
        "previous head retires atomically",
    )?;
    expect(
        first.discard_step(64, control).is_err(),
        "staging cannot delete a retained generation",
    )?;
    expect_eq(
        &loser_stage.status(control)?,
        &Some(DiskANNStageStatus::Sealed),
        "losing candidate stays sealed",
    )?;
    expect_eq(
        &selected_generation(scope, &*retained.unwrap(), control)?,
        &Some(first.generation()),
        "retained reader sees matching old head and state",
    )
}

fn private_and_definition(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    repository: &KeyValueDiskANNStore,
    scope: &DiskANNIndexScope,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNGeneration> {
    store.begin_transaction()?;
    store.savepoint("publication_input")?;
    canonical.replace(2, &[vec![0.0, 0.0]], control)?;
    let (undone, _) = build(canonical, repository, control)?;
    store.rollback_to_savepoint("publication_input")?;
    expect(
        publish(store, &undone, control).is_err(),
        "undone private input cannot publish",
    )?;
    store.rollback_transaction()?;
    store.begin_transaction()?;
    canonical.replace(2, &[vec![0.0, 0.0]], control)?;
    let (private, private_stage) = build(canonical, repository, control)?;
    publish(store, &private, control)?;
    store.commit_transaction()?;
    expect_eq(
        &selected(store, scope, control)?,
        &Some(private_stage.generation()),
        "same-transaction canonical changes and head commit together",
    )?;
    let (conflicted, conflicted_stage) = build(canonical, repository, control)?;
    store.begin_transaction()?;
    publish(store, &conflicted, control)?;
    KeyValueCatalog::new(store.open_session()?).save_catalog_index_row(&row([91; 16])?)?;
    expect(
        store.commit_transaction().is_err(),
        "catalog condition survives evaluation until commit",
    )?;
    store.rollback_transaction()?;
    expect_eq(
        &conflicted_stage.status(control)?,
        &Some(DiskANNStageStatus::Sealed),
        "definition conflict rolls back generation state",
    )?;
    expect_eq(
        &selected(store, scope, control)?,
        &Some(private_stage.generation()),
        "definition conflict preserves prior head",
    )?;
    Ok(private_stage.generation())
}

/// Check the durable head and complete origin artifact after an actual cold reopen.
pub fn verify_diskann_publication_reopen(
    store: &Arc<dyn KeyValueStore>,
    expected: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let canonical = KeyValueDiskANNCanonical::new(store.clone(), TABLE, FIELD, 2)?;
    let source = canonical.retain_for_index(&row([91; 16])?.relation, &control)?;
    let scope = source.index_scope(&Resolver, &control)?;
    expect_eq(
        &selected(store, &scope, &control)?,
        &Some(expected),
        "cold reopened head",
    )?;
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    let reader =
        DiskANNOriginReader::open(repository.open_source(expected, &control)?, 8192, &control)?;
    for document in [1, 2] {
        expect_eq(
            &reader
                .origin(document, &control)?
                .map(crate::diskann_index::format::DiskANNCanonicalOrigin::version),
            &source.origin(document, &control)?,
            "reopened complete published coverage",
        )?;
    }
    Ok(())
}

/// Private publication and retained private snapshots own the sealed build after its staging adapters close.
pub fn verify_diskann_publication_ownership(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let canonical = setup(store)?;
    KeyValueCatalog::new(store.clone()).save_catalog_index_row(&row([91; 16])?)?;
    canonical.replace(1, &[vec![1.0, 0.0]], &control)?;
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    repository.initialize(&control)?;
    let (coverage, stage) = build(&canonical, &repository, &control)?;
    let generation = stage.generation();
    store.begin_transaction()?;
    publish(store, &coverage, &control)?;
    drop((coverage, stage, repository, canonical));
    let cleaner = KeyValueDiskANNStore::connect(store, &control)?;
    expect(
        !cleaner.reclaim_abandoned_step(generation, 64, &control)?,
        "private publication retains the actual sealed source",
    )?;
    let held = store.open_retained_read_session(control.cancellation())?;
    store.rollback_transaction()?;
    expect(
        !cleaner.reclaim_abandoned_step(generation, 64, &control)?,
        "retained private snapshot still owns the source after rollback",
    )?;
    drop(held);
    expect(
        cleaner.reclaim_abandoned_step(generation, 64, &control)?,
        "final private owner release admits orphan cleanup",
    )
}
