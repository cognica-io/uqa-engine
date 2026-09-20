//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare index registry changes before publishing their owning table definitions.

use crate::catalog::{identity::CatalogIdentityReservationContext, index::index_definition};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{ColumnDef, IndexKey, TableConstraintSet},
    catalog::index::{IndexCatalogIdentity, IndexDefinition},
    schema::constraint_metadata::{CatalogObjectAllocator, CatalogOidClass},
};
use uqa_storage::{CatalogIndexRow, StorageBackendError, StorageBackendResult};

pub mod binding;
pub(super) mod builds;
pub(super) mod constraints;
pub(crate) mod foreign_keys;
pub mod lifecycle;
mod names;
mod owners;
pub(super) mod partitions;
mod recheck;
pub(super) mod schema;
pub(crate) mod validation;

pub trait IndexRegistryPublication {
    fn persist_index(&self, row: &CatalogIndexRow) -> StorageBackendResult<()>;
    fn erase_index(&self, row: &CatalogIndexRow) -> StorageBackendResult<()>;
    fn publish_index(&self, row: CatalogIndexRow);
    fn forget_index(&self, relation: &RelationIdentity);
    fn refresh_index_table(&self, table: &str) -> StorageBackendResult<()>;
}

#[derive(Clone, Copy)]
pub struct IndexRegistryContext<'a> {
    pub identities: CatalogIdentityReservationContext<'a>,
    pub publication: &'a dyn IndexRegistryPublication,
    pub builds: &'a dyn super::creation::IndexCreationPublication,
    pub vectors: &'a dyn uqa_sql::schema::indexes::vectors::VectorIndexCatalog,
    pub tables: &'a dyn crate::schema::publication::TableSchemaCatalog,
    pub locks: &'a dyn crate::row_locks::binding::RelationLockSession,
    pub lock_catalog: &'a dyn crate::row_locks::binding::RelationLockCatalog,
}

#[derive(Default)]
pub struct IndexRegistryChange {
    pub upserts: Vec<CatalogIndexRow>,
    pub removals: Vec<CatalogIndexRow>,
    schema: Vec<schema::OwnerChange>,
    derived_builds: Vec<CatalogIndexRow>,
}

impl IndexRegistryChange {
    /// A name change preserves every physical key and must not hydrate the indexed table.
    pub(super) fn rename(
        context: &IndexRegistryContext<'_>,
        previous: &CatalogIndexRow,
        renamed: CatalogIndexRow,
    ) -> StorageBackendResult<()> {
        context.publication.erase_index(previous)?;
        context.publication.persist_index(&renamed)?;
        context.publication.forget_index(&previous.relation);
        context.publication.publish_index(renamed);
        Ok(())
    }

    /// All identity reservations and schema candidate writes precede this non-refreshing publication path. The caller's schema transaction restores both registries if persistence or physical hydration fails.
    pub fn publish(self, context: &IndexRegistryContext<'_>) -> StorageBackendResult<()> {
        let publication = context.publication;
        let mut tables = BTreeSet::new();
        for row in &self.derived_builds {
            builds::build(context, row)?;
        }
        for change in &self.schema {
            let state = change.current(context.tables)?;
            state.persist_candidate(&change.columns, &change.constraints)?;
            tables.insert(change.relation.qualified_name());
        }
        for row in &self.removals {
            publication.erase_index(row)?;
            tables.insert(row.table_name.clone());
        }
        for row in &self.upserts {
            publication.persist_index(row)?;
            tables.insert(row.table_name.clone());
        }
        for change in self.schema {
            let state = change.current(context.tables)?;
            state.publish_constraints(change.columns, change.constraints);
        }
        for row in self.removals {
            publication.forget_index(&row.relation);
        }
        for row in self.upserts {
            publication.publish_index(row);
        }
        for table in tables {
            publication.refresh_index_table(&table)?;
        }
        Ok(())
    }
}

pub fn prepare_constraint_indexes(
    context: &IndexRegistryContext<'_>,
    table: &str,
    table_object_id: [u8; 16],
    columns: &mut [ColumnDef],
    constraints: &mut TableConstraintSet,
) -> StorageBackendResult<IndexRegistryChange> {
    let relation = RelationIdentity::from_legacy_name(table).map_err(invalid)?;
    let catalog = context.identities.catalog.current_catalog_snapshot();
    let previous = &catalog.snapshot().definitions.catalog_indexes;
    let mut allocator = context
        .identities
        .allocator(crate::catalog::identity::allocate_catalog_object_id);
    let mut candidate = catalog.snapshot().clone();
    schema::replace(&mut candidate, &relation, columns, constraints)?;
    let mut schema = schema::prepare_descendants(
        &catalog,
        &mut candidate,
        &relation,
        &mut allocator,
        |name| {
            let state = context
                .tables
                .table_state(&name.qualified_name())?
                .ok_or_else(|| invalid("partition disappeared"))?;
            Ok((state.columns(), state.constraints(), state.object_id()))
        },
    )?;
    let candidate = crate::catalog::CatalogReadView::new(candidate);
    let mut rows = previous.as_ref().clone();
    for (name, columns, constraints) in std::iter::once((&relation, &*columns, &*constraints))
        .chain(schema.iter().map(|change| {
            (
                &change.relation,
                change.columns.as_slice(),
                &change.constraints,
            )
        }))
    {
        let change = constraints::prepare(
            &catalog,
            &name.qualified_name(),
            candidate.snapshot().tables[name].object_id,
            columns,
            constraints,
            &mut allocator,
        )?;
        for removed in change.removals {
            rows.remove(&removed.relation);
        }
        for row in change.upserts {
            rows.insert(row.relation.clone(), row);
        }
    }
    partitions::detach(&candidate, &mut rows)?;
    partitions::materialize(&candidate, &mut rows, &mut allocator)?;
    foreign_keys::bind(&rows, columns, constraints, true)?;
    for change in &mut schema {
        foreign_keys::bind(&rows, &mut change.columns, &mut change.constraints, true)?;
    }
    validation::validate(&candidate, &rows)?;
    let mut change = difference(previous, &rows)?;
    change.schema = schema;
    if catalog.snapshot().tables[&relation].object_id != table_object_id {
        return Err(invalid("index owner changed before preparation"));
    }
    names::reserve_new_names(context, previous, &rows)?;
    recheck::validate(context, &catalog, &candidate, &rows, &relation)?;
    Ok(change)
}

fn difference(
    previous: &BTreeMap<RelationIdentity, CatalogIndexRow>,
    rows: &BTreeMap<RelationIdentity, CatalogIndexRow>,
) -> StorageBackendResult<IndexRegistryChange> {
    Ok(IndexRegistryChange {
        upserts: rows
            .iter()
            .filter(|(name, row)| previous.get(*name).is_none_or(|old| !same_row(old, row)))
            .map(|(_, row)| row.clone())
            .collect(),
        removals: previous
            .iter()
            .filter(|(name, old)| {
                rows.get(*name)
                    .is_none_or(|row| row.table_name != old.table_name)
            })
            .map(|(_, row)| row.clone())
            .collect(),
        schema: Vec::new(),
        derived_builds: builds::new_descendants(previous, rows)?,
    })
}

fn same_row(left: &CatalogIndexRow, right: &CatalogIndexRow) -> bool {
    left.relation == right.relation
        && left.table_name == right.table_name
        && left.index_type == right.index_type
        && left.columns_json == right.columns_json
        && left.parameters_json == right.parameters_json
        && left.definition_json == right.definition_json
}

fn invalid(error: impl ToString) -> StorageBackendError {
    StorageBackendError::Other(error.to_string())
}

fn metadata_error(
    error: uqa_sql::schema::constraint_metadata::ConstraintMetadataError,
) -> StorageBackendError {
    StorageBackendError::backend("index registry", error)
}

/// Construct only partition indexes newly introduced by an initial catalog conversion. Existing stored physical indexes keep the strict restore path.
pub fn build_restored_partition_indexes(
    context: &IndexRegistryContext<'_>,
    rows: &[CatalogIndexRow],
) -> StorageBackendResult<()> {
    for row in rows {
        builds::build(context, row)?;
    }
    Ok(())
}
