//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select newly allocated physical indexes and preserve their catalog/build ordering.

use super::{
    index_definition, BTreeMap, BTreeSet, CatalogIndexRow, IndexRegistryContext, RelationIdentity,
    StorageBackendError, StorageBackendResult,
};

#[cfg(test)]
std::thread_local! {
    pub(super) static PHYSICAL_BUILD_SELECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(in crate::schema::indexes) fn new_physical_indexes(
    previous: &BTreeMap<RelationIdentity, CatalogIndexRow>,
    rows: &BTreeMap<RelationIdentity, CatalogIndexRow>,
) -> StorageBackendResult<Vec<CatalogIndexRow>> {
    #[cfg(test)]
    PHYSICAL_BUILD_SELECTIONS.set(PHYSICAL_BUILD_SELECTIONS.get() + 1);
    let identities = previous
        .values()
        .map(|row| {
            Ok(index_definition(row)?
                .catalog
                .map(|identity| identity.identity.object_id))
        })
        .collect::<StorageBackendResult<BTreeSet<_>>>()?;
    let mut builds = Vec::new();
    for row in rows.values() {
        let definition = index_definition(row)?;
        if (definition.relationships.parent_index.is_some()
            || row.index_type.eq_ignore_ascii_case("diskann"))
            && !identities.contains(
                &definition
                    .catalog
                    .map(|identity| identity.identity.object_id),
            )
        {
            builds.push(row.clone());
        }
    }
    Ok(builds)
}

pub(super) fn build(
    context: &IndexRegistryContext<'_>,
    row: &CatalogIndexRow,
) -> StorageBackendResult<()> {
    if row.index_type.eq_ignore_ascii_case("diskann") {
        return super::super::diskann::build(context.vectors, context.builds, row);
    }
    let statement = uqa_sql::catalog::index::stored::declaration(row)?;
    super::super::creation::build_physical_index(
        context.vectors,
        context.builds,
        &statement,
        &row.index_type,
    )
    .map_err(|error| StorageBackendError::backend("partition index build", error))
}

/// A name or identity reservation can refresh the catalog between a physical build and its registry publication. Only the completed registry can select the live writer's final binding.
pub(super) fn rebind(
    context: &IndexRegistryContext<'_>,
    row: &CatalogIndexRow,
) -> StorageBackendResult<()> {
    if !row.index_type.eq_ignore_ascii_case("ivf") && !row.index_type.eq_ignore_ascii_case("hnsw") {
        return Ok(());
    }
    let statement = uqa_sql::catalog::index::stored::declaration(row)?;
    let target = uqa_sql::schema::indexes::vectors::resolve_vector_index_columns(
        context.vectors,
        &statement,
        &row.index_type,
    )
    .map_err(|error| StorageBackendError::backend("bind published vector index", error))?;
    for (field, dimensions) in target.fields {
        let spec = crate::catalog::index::vectors::stored_spec(row, dimensions)?;
        context.publication.rebind_persistent_vector_field(
            &target.table,
            field,
            dimensions,
            spec,
        )?;
    }
    Ok(())
}
