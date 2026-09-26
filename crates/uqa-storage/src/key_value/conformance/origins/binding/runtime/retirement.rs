//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{
    identity::{row, Resolver},
    setup, FIELD, TABLE,
};
use super::{canonical, diskann_runtime_fixture_options, generation, runtime, scores};
use crate::diskann_index::{build::DiskANNTemporaryBudget, format::DiskANNGeneration};
use crate::key_value::conformance::{expect, expect_eq};
use crate::key_value::{DiskANNStageStatus, KeyValueDiskANNStore};
use crate::read_control::StorageReadControl;
use crate::{
    CatalogFacade, KeyValueCatalog, KeyValueStore, KeyValueVectorIndex, StorageBackendResult,
    VectorIndex,
};
use std::sync::Arc;

/// Retire committed and privately created heads, undo deletion, and recreate the same SQL name with a different immutable identity. Call on a fresh disposable store.
pub fn verify_diskann_runtime_retirement(
    store: &Arc<dyn KeyValueStore>,
    private: bool,
) -> StorageBackendResult<(DiskANNGeneration, DiskANNGeneration)> {
    let control = StorageReadControl::with_limit(1 << 21);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let options = diskann_runtime_fixture_options(2)?;
    setup(store)?;
    let catalog = KeyValueCatalog::new(store.clone());
    let definition = row([91; 16])?;
    catalog.save_catalog_index_row(&definition)?;
    let mut raw = KeyValueVectorIndex::new(store.clone(), TABLE, FIELD, 2);
    raw.add(1, vec![1.0, 0.0])?;
    raw.add_many(2, vec![vec![-1.0, 0.0], vec![0.0, 1.0]])?;
    expect(
        canonical(store)?
            .retire_index(&definition.relation, &Resolver, &control)
            .is_err(),
        "retirement requires the caller transaction",
    )?;
    store.begin_transaction()?;
    canonical(store)?.create_index(
        &definition.relation,
        &Resolver,
        options,
        &temporary,
        &control,
    )?;
    if !private {
        store.commit_transaction()?;
        store.begin_transaction()?;
    }
    let mut stale = runtime(store, &temporary, &control)?;
    if private {
        let obsolete = canonical(store)?.retain_for_index(&definition.relation, &control)?;
        stale.initialize()?;
        expect(
            store
                .with_mutation(&mut |read, batch| {
                    obsolete
                        .retire_generation(&Resolver, read, batch, &control)
                        .map(|_| ())
                })
                .is_err(),
            "retirement cannot refresh a superseded captured head",
        )?;
    }
    let held = stale.snapshot()?;
    let first = generation(store, &control)?;
    store.put(b"retirement-outer-write", b"kept")?;
    store.savepoint("before-retirement")?;
    canonical(store)?.retire_index(&definition.relation, &Resolver, &control)?;
    expect(
        stale.snapshot().is_err(),
        "retired live selection is not queryable",
    )?;
    expect(
        stale.add(9, vec![1.0, 0.0]).is_err(),
        "retired live selection is not writable",
    )?;
    expect_eq(&raw.count()?, &3, "retirement preserves raw ordinals")?;
    catalog.drop_catalog_index(&definition.relation)?;
    scores(&*held, &[(1, 1.0), (2, 0.0)])?;
    store.rollback_to_savepoint("before-retirement")?;
    expect_eq(
        &generation(store, &control)?,
        &first,
        "undo restores original selected head",
    )?;
    scores(&stale, &[(1, 1.0), (2, 0.0)])?;
    canonical(store)?.retire_index(&definition.relation, &Resolver, &control)?;
    catalog.drop_catalog_index(&definition.relation)?;
    catalog.save_catalog_index_row(&row([92; 16])?)?;
    canonical(store)?.create_index(
        &definition.relation,
        &Resolver,
        options,
        &temporary,
        &control,
    )?;
    let replacement = generation(store, &control)?;
    expect(
        replacement.index() != first.index(),
        "recreation has a new physical incarnation",
    )?;
    expect(
        stale.add(9, vec![1.0, 0.0]).is_err(),
        "old handle cannot bind the reused SQL name",
    )?;
    store.commit_transaction()?;
    expect_eq(
        &store.get(b"retirement-outer-write")?,
        &Some(b"kept".to_vec()),
        "retirement keeps outer effects",
    )?;
    scores(&*held, &[(1, 1.0), (2, 0.0)])?;
    verify_diskann_runtime_retirement_reopen(store, (first, replacement))?;
    Ok((first, replacement))
}

/// Check actual durable retirement and replacement selection after all original provider owners close.
pub fn verify_diskann_runtime_retirement_reopen(
    store: &Arc<dyn KeyValueStore>,
    generations: (DiskANNGeneration, DiskANNGeneration),
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 21);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    expect_eq(
        &repository
            .resume_stage(generations.0, &control)?
            .status(&control)?,
        &Some(DiskANNStageStatus::Retired),
        "private or committed old generation is durably retired",
    )?;
    expect_eq(
        &generation(store, &control)?,
        &generations.1,
        "reopen selects the replacement",
    )?;
    scores(
        &runtime(store, &temporary, &control)?,
        &[(1, 1.0), (2, 0.0)],
    )
}
