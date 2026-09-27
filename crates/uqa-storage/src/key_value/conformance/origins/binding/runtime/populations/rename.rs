//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column movement cannot merge independently selected canonical populations.

use super::{
    canonical, expect, expect_eq, field_prefixes, publish, row, Arc, CatalogFacade,
    DiskANNCanonicalCounts, DiskANNCanonicalRead, DiskANNGeneration, KeyValueCatalog,
    KeyValueDiskANNStore, KeyValueStore, Resolver, StorageBackendResult, StorageReadControl, FIELD,
    TABLE,
};
use crate::key_value::{
    codec, conformance::build_diskann_publication_fixture, vector_index::origin,
    KeyValueDiskANNCanonical, KeyValueVectorIndex,
};

use crate::VectorIndex;

const DESTINATION: &str = "occupied_vector";

pub(super) fn reject_destinations(
    store: &Arc<dyn KeyValueStore>,
    generation: DiskANNGeneration,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    raw_destination_preservation(store)?;
    catalog_owned_destinations(store)?;
    let catalog = KeyValueCatalog::new(store.clone());
    store.begin_transaction()?;
    let second = second_generation(store, control)?;
    let before = store.scan_prefix(b"")?;
    expect(
        catalog
            .rename_column_data(TABLE, FIELD, DESTINATION)
            .is_err(),
        "column rename rejects distinct generations in one destination namespace",
    )?;
    expect_eq(
        &store.scan_prefix(b"")?,
        &before,
        "rejected rename preserves both complete fields",
    )?;
    let target = KeyValueDiskANNCanonical::new(store.clone(), TABLE, DESTINATION, 2)?;
    expect_eq(
        &target.retain(control)?.population_counts(second, control)?,
        &Some(DiskANNCanonicalCounts::new(1, 0)?),
        "destination header keeps its independent population",
    )?;
    catalog.rename_column_data(TABLE, FIELD, FIELD)?;
    expect_eq(
        &store.scan_prefix(b"")?,
        &before,
        "same-name storage movement is inert",
    )?;
    store.rollback_transaction()?;
    for (source, destination) in prefixes(FIELD)?.into_iter().zip(prefixes(DESTINATION)?) {
        let entry = store
            .scan_prefix(&source)?
            .into_iter()
            .next()
            .expect("seeded namespace");
        let mut key = destination;
        key.extend_from_slice(&entry.0[source.len()..]);
        store.begin_transaction()?;
        store.put(&key, &entry.1)?;
        let before = store.scan_prefix(b"")?;
        expect(
            catalog
                .rename_column_data(TABLE, FIELD, DESTINATION)
                .is_err(),
            "every canonical, origin, journal, header and witness namespace blocks a collision",
        )?;
        expect_eq(
            &store.scan_prefix(b"")?,
            &before,
            "partial destination rejection stages no writes",
        )?;
        store.rollback_transaction()?;
    }
    expect_eq(
        &canonical(store)?
            .retain(control)?
            .population_counts(generation, control)?,
        &Some(DiskANNCanonicalCounts::new(3, 0)?),
        "all rejected movements preserve the selected source population",
    )
}

fn prefixes(field: &str) -> StorageBackendResult<[Vec<u8>; 5]> {
    let [header, witnesses] = field_prefixes(TABLE, field)?;
    Ok([
        codec::vector_field_prefix(TABLE, field)?,
        origin::prefix(TABLE, field)?,
        origin::journal::prefix(TABLE, field)?,
        header,
        witnesses,
    ])
}

fn second_generation(
    store: &Arc<dyn KeyValueStore>,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNGeneration> {
    let catalog = KeyValueCatalog::new(store.clone());
    let mut table = catalog
        .load_tables()?
        .into_iter()
        .find(|table| table.relation.qualified_name() == TABLE)
        .expect("seeded table");
    table.vector_fields.push(crate::VectorFieldSchema {
        field: DESTINATION.into(),
        dimensions: 2,
    });
    catalog.save_table(&table)?;
    let mut definition = row([92; 16])?;
    definition.relation.name.push_str("_destination");
    definition.columns_json = serde_json::to_string(&[DESTINATION])?;
    catalog.save_catalog_index_row(&definition)?;
    let canonical = KeyValueDiskANNCanonical::new(store.clone(), TABLE, DESTINATION, 2)?;
    canonical.replace(9, &[vec![0.0, 1.0]], control)?;
    canonical.replace(10, &[], control)?;
    let source = canonical.retain_for_index(&definition.relation, control)?;
    let scope = source.index_scope(&Resolver, control)?;
    let parameters = source.index_parameters().expect("bound second field");
    let repository = KeyValueDiskANNStore::connect(store, control)?;
    let mut stage = repository.allocate_bound_stage(&scope, control)?;
    let coverage = build_diskann_publication_fixture(source, &mut stage, parameters, control)?;
    publish(store, &coverage, control)?;
    Ok(stage.generation())
}

fn raw_destination_preservation(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    store.begin_transaction()?;
    let mut source = KeyValueVectorIndex::new(store.clone(), TABLE, "raw_before", 2);
    let mut target = KeyValueVectorIndex::new(store.clone(), TABLE, "raw_after", 2);
    source.add(1, vec![1.0, 0.0])?;
    source.add(3, vec![1.0, 0.0])?;
    target.add(1, vec![0.0, 1.0])?;
    target.add(2, vec![-1.0, 0.0])?;
    let preserved = store.scan_prefix(&codec::vector_field_prefix(TABLE, "raw_after")?)?;
    KeyValueCatalog::new(store.clone()).rename_column_data(TABLE, "raw_before", "raw_after")?;
    expect_eq(
        &source.count()?,
        &0,
        "raw source moves out of its old field",
    )?;
    expect_eq(
        &target.count()?,
        &3,
        "ordinary raw backfill merges disjoint documents",
    )?;
    for (key, value) in preserved {
        expect_eq(
            &store.get(&key)?,
            &Some(value),
            "ordinary raw destination keeps existing values",
        )?;
    }
    store.rollback_transaction()
}

fn catalog_owned_destinations(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(store.clone());
    for field in ["legacy_before", "legacy_after"] {
        store.begin_transaction()?;
        let mut target = KeyValueVectorIndex::new(store.clone(), TABLE, "legacy_after", 2);
        target.add(1, vec![1.0, 0.0])?;
        let mut definition = row([93; 16])?;
        definition.relation.name.push_str("_legacy");
        definition.columns_json = serde_json::to_string(&[field])?;
        catalog.save_catalog_index_row(&definition)?;
        let before = store.scan_prefix(b"")?;
        let result = catalog.rename_column_data(TABLE, "legacy_before", "legacy_after");
        expect(
            result.is_err_and(|error| {
                error
                    .to_string()
                    .contains("column rename would merge DiskANN canonical fields")
            }),
            "catalog ownership rejects a predecessor field without origin or population records",
        )?;
        expect_eq(
            &store.scan_prefix(b"")?,
            &before,
            "catalog-owned rejection preserves the complete storage view",
        )?;
        store.rollback_transaction()?;
    }
    Ok(())
}
