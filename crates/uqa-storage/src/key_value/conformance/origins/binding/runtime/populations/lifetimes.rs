//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    canonical, counts, expect, expect_eq, field_prefixes, handle, seed, Arc, CatalogFacade,
    DiskANNCanonicalCounts, DiskANNCanonicalRead, DiskANNGeneration, DiskANNPersistentOwner,
    KeyValueCatalog, KeyValueStore, StorageBackendResult, StorageReadControl, FIELD, TABLE,
};
use crate::key_value::{codec, KeyValueDiskANNCanonical, KeyValueDiskANNPopulationRecords};
use crate::mvcc::{DiskANNPopulationRecordLayout, VersionError};

/// Verify corruption rejection and atomic structural movement with later writes, then reclaim population tombstones after the last retained view closes.
pub fn verify_diskann_population_lifetimes(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 22);
    let generation = seed(store, &control)?;
    super::rename::reject_destinations(store, generation, &control)?;
    corruption(store, generation, &control)?;
    structural(store, generation, &control)?;
    store.vacuum()?;
    Ok(())
}

fn corruption(
    store: &Arc<dyn KeyValueStore>,
    generation: DiskANNGeneration,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let live = handle(store, control)?;
    let field = codec::vector_field_prefix(TABLE, FIELD)?;
    let header = KeyValueDiskANNPopulationRecords::header_key(&field, generation, control)
        .map_err(VersionError::into_storage_error)?;
    store.begin_transaction()?;
    store.put(&header, b"invalid population")?;
    expect(
        live.query_metadata().is_err(),
        "malformed fixed header is an error",
    )?;
    expect(
        live.replace(1, &[]).is_err(),
        "corrupt header rejects the complete mutation",
    )?;
    store.rollback_transaction()?;
    counts(&live, 3, 0)?;
    let witness = KeyValueDiskANNPopulationRecords
        .witness_key(&header, 3, control)
        .map_err(VersionError::into_storage_error)?;
    store.begin_transaction()?;
    store.delete(&witness)?;
    expect(
        live.replace(3, &[vec![1.0, 0.0]]).is_err(),
        "even an empty origin requires its preceding witness",
    )?;
    expect_eq(
        &canonical(store)?
            .retain(control)?
            .origin(3, control)?
            .is_some(),
        &true,
        "rejected write preserves the empty origin",
    )?;
    counts(&live, 3, 0)?;
    store.rollback_transaction()?;
    let held = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())?;
    expect(
        live.replace(1, &[]).is_err(),
        "original memory exhaustion preserves the origin and population",
    )?;
    drop(held);
    counts(&live, 3, 0)?;
    Ok(())
}

fn structural(
    store: &Arc<dyn KeyValueStore>,
    generation: DiskANNGeneration,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(store.clone());
    let original = canonical(store)?.retain(control)?;
    let destination = "public.population_renamed";
    store.begin_transaction()?;
    catalog.rename_column_data(TABLE, FIELD, "after")?;
    catalog.rename_table_data(TABLE, destination)?;
    let renamed = KeyValueDiskANNCanonical::new(store.clone(), destination, "after", 2)?;
    require(&renamed, generation, 3, 0, control)?;
    renamed.replace(1, &[vec![1.0, 0.0], vec![0.0, 1.0]], control)?;
    require(&renamed, generation, 4, 2, control)?;
    store.savepoint("population-structural")?;
    catalog.drop_column_data(destination, "after")?;
    expect_eq(
        &renamed
            .retain(control)?
            .population_counts(generation, control)?,
        &None,
        "private column removal deletes the header",
    )?;
    store.rollback_to_savepoint("population-structural")?;
    require(&renamed, generation, 4, 2, control)?;
    store.commit_transaction()?;
    require(&renamed, generation, 4, 2, control)?;
    expect_eq(
        &original.population_counts(generation, control)?,
        &Some(DiskANNCanonicalCounts::new(3, 0)?),
        "renaming and mutation preserve the original retained metadata",
    )?;
    for prefix in field_prefixes(TABLE, FIELD)? {
        expect(
            store.scan_prefix(&prefix)?.is_empty(),
            "rename leaves no population in the old namespace",
        )?;
    }
    store.begin_transaction()?;
    catalog.purge_table_data(destination)?;
    expect_eq(
        &renamed
            .retain(control)?
            .population_counts(generation, control)?,
        &None,
        "table purge removes derived metadata",
    )?;
    store.rollback_transaction()?;
    require(&renamed, generation, 4, 2, control)?;
    catalog.drop_column_data(destination, "after")?;
    store.vacuum()?;
    expect_eq(
        &original.population_counts(generation, control)?,
        &Some(DiskANNCanonicalCounts::new(3, 0)?),
        "vacuum cannot remove the retained old population",
    )?;
    for prefix in field_prefixes(destination, "after")? {
        expect(
            store.scan_prefix(&prefix)?.is_empty(),
            "column removal deletes every witness and header",
        )?;
    }
    Ok(())
}

fn require(
    owner: &KeyValueDiskANNCanonical,
    generation: DiskANNGeneration,
    current: u64,
    changed: u64,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    expect_eq(
        &owner
            .retain(control)?
            .population_counts(generation, control)?,
        &Some(DiskANNCanonicalCounts::new(current, changed)?),
        "structural copy and later replacement preserve population sums",
    )
}
