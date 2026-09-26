//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use super::{definition, expect, expect_eq, guard, setup, FIELD, TABLE};
use crate::diskann_index::catalog::DiskANNIndexResolver;
use crate::diskann_index::format::DiskANNGeneration;
use crate::key_value::KeyValueDiskANNCanonical;
use crate::key_value::KeyValueDiskANNStore;
use crate::read_control::StorageReadControl;
use crate::{
    CatalogFacade, CatalogIndexRow, KeyValueCatalog, KeyValueStore, RelationIdentity,
    StorageBackendResult,
};

// Fixture-only interpretation: provider conformance treats SQL definitions as opaque.
struct Resolver;

impl DiskANNIndexResolver for Resolver {
    fn resolve(
        &self,
        definition: &str,
        table: [u8; 16],
        control: &StorageReadControl,
    ) -> StorageBackendResult<[u8; 16]> {
        control.check()?;
        expect_eq(&table, &[79; 16], "resolver receives the captured table")?;
        serde_json::from_str(definition).map_err(Into::into)
    }
}

fn row(identity: [u8; 16]) -> StorageBackendResult<CatalogIndexRow> {
    let mut row = definition()?;
    row.definition_json = Some(serde_json::to_string(&identity)?);
    Ok(row)
}

/// Verify real captured catalog definitions, independent durable handles, private views and original controls. The returned generation can be checked after all provider owners close.
pub fn verify_diskann_catalog_identity(
    store: &Arc<dyn KeyValueStore>,
    foreign: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<DiskANNGeneration> {
    let capture = StorageReadControl::with_limit(1 << 20);
    let control = StorageReadControl::with_limit(1 << 20);
    let canonical = setup(store)?;
    let catalog = KeyValueCatalog::new(store.clone());
    let row = row([91; 16])?;
    catalog.save_catalog_index_row(&row)?;
    let source = canonical.retain_for_index(&row.relation, &capture)?;
    let scope = source.index_scope(&Resolver, &control)?;
    expect_eq(&scope.table_object(), &[79; 16], "actual table identity")?;
    expect_eq(
        &scope.storage_generation(),
        &[80; 16],
        "actual storage generation",
    )?;
    expect_eq(
        &scope.index_object(),
        &[91; 16],
        "actual retained index definition",
    )?;
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    repository.initialize(&control)?;
    let mut stage = repository.allocate_bound_stage(&scope, &control)?;
    stage.start(&control)?;
    let first = stage.generation();
    let second = repository
        .allocate_bound_stage(&scope, &control)?
        .generation();
    same_handles(first, second)?;
    let other = KeyValueDiskANNStore::connect(foreign, &control)?;
    other.initialize(&control)?;
    expect(
        other.allocate_bound_stage(&scope, &control).is_err(),
        "foreign history cannot adopt catalog scope",
    )?;
    drop((stage, repository));
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    let third = repository
        .allocate_bound_stage(&scope, &control)?
        .generation();
    same_handles(second, third)?;
    private_identity(store, &canonical, &repository, &control)?;
    let final_generation = changed_definitions(store, &canonical, first, &control)?;
    expect_eq(
        &source.index_scope(&Resolver, &control)?.index_object(),
        &[91; 16],
        "resolution never follows the live catalog",
    )?;
    expect(
        guard(store, &source, &control).is_err(),
        "physical handle allocation does not waive the definition guard",
    )?;
    capture.cancellation().cancel();
    expect(
        repository.allocate_bound_stage(&scope, &control).is_err(),
        "scope retains the original capture cancellation",
    )?;
    Ok(final_generation)
}

fn private_identity(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    repository: &KeyValueDiskANNStore,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    store.begin_transaction()?;
    let row = row([93; 16])?;
    KeyValueCatalog::new(store.clone()).save_catalog_index_row(&row)?;
    let source = canonical.retain_for_index(&row.relation, control)?;
    let scope = source.index_scope(&Resolver, control)?;
    repository.allocate_bound_stage(&scope, control)?;
    expect(
        store.in_transaction(),
        "staging does not finish the private catalog transaction",
    )?;
    store.rollback_transaction()?;
    expect_eq(
        &source.index_scope(&Resolver, control)?.index_object(),
        &[93; 16],
        "private source survives undo without following committed definitions",
    )?;
    expect(
        guard(store, &source, control).is_err(),
        "undone definition cannot publish its prepared generation",
    )
}

fn same_handles(before: DiskANNGeneration, after: DiskANNGeneration) -> StorageBackendResult<()> {
    expect_eq(
        &(after.database(), after.table(), after.index()),
        &(before.database(), before.table(), before.index()),
        "incarnation handles survive another staging session",
    )?;
    expect(
        after.generation() > before.generation(),
        "generation numbers are never reused",
    )
}

fn changed_definitions(
    store: &Arc<dyn KeyValueStore>,
    canonical: &KeyValueDiskANNCanonical,
    first: DiskANNGeneration,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNGeneration> {
    let repository = KeyValueDiskANNStore::connect(store, control)?;
    let catalog = KeyValueCatalog::new(store.clone());
    let mut row = row([91; 16])?;
    catalog.drop_catalog_index(&row.relation)?;
    row.relation.name = "diskann_renamed".into();
    catalog.save_catalog_index_row(&row)?;
    let scope = canonical
        .retain_for_index(&row.relation, control)?
        .index_scope(&Resolver, control)?;
    same_handles(
        first,
        repository
            .allocate_bound_stage(&scope, control)?
            .generation(),
    )?;
    let mut replacement = [91; 16];
    replacement[0] ^= 1;
    catalog.drop_catalog_index(&row.relation)?;
    row.definition_json = Some(serde_json::to_string(&replacement)?);
    catalog.save_catalog_index_row(&row)?;
    let scope = canonical
        .retain_for_index(&row.relation, control)?
        .index_scope(&Resolver, control)?;
    let replaced = repository
        .allocate_bound_stage(&scope, control)?
        .generation();
    expect_eq(
        &replaced.table(),
        &first.table(),
        "new index keeps the table incarnation",
    )?;
    expect(
        replaced.index() != first.index(),
        "full index identities distinguish recreated names",
    )?;
    let mut table = catalog
        .load_tables()?
        .into_iter()
        .find(|table| table.relation.qualified_name() == TABLE)
        .ok_or_else(|| crate::StorageBackendError::Other("missing fixture table".into()))?;
    table.storage_generation[0] ^= 1;
    catalog.save_table(&table)?;
    let source = canonical.retain_for_index(&row.relation, control)?;
    let scope = source.index_scope(&Resolver, control)?;
    let mut stage = repository.allocate_bound_stage(&scope, control)?;
    let generation = stage.generation();
    expect(
        generation.table() != first.table() && generation.index() != replaced.index(),
        "full storage generation changes both physical namespaces",
    )?;
    stage.start(control)?;
    Ok(generation)
}

/// Reopen persistent mappings and allocate the next distinct generation after all previous sessions and scopes have closed.
pub fn verify_diskann_catalog_identity_reopen(
    store: &Arc<dyn KeyValueStore>,
    before: DiskANNGeneration,
) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(1 << 20);
    let canonical = KeyValueDiskANNCanonical::new(store.clone(), TABLE, FIELD, 2)?;
    let source = canonical.retain_for_index(
        &RelationIdentity::new("public", "diskann_renamed"),
        &control,
    )?;
    let scope = source.index_scope(&Resolver, &control)?;
    let repository = KeyValueDiskANNStore::connect(store, &control)?;
    same_handles(
        before,
        repository
            .allocate_bound_stage(&scope, &control)?
            .generation(),
    )
}
