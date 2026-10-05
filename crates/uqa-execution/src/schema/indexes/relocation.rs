//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relocate an owner's index names without rebuilding physical keys or replacing their identities.

use super::registry::{IndexRegistryChange, IndexRegistryContext, IndexRegistryPublication};
use crate::row_locks::RelationLockMode;
use crate::schema::namespaces::relations::RelationCreationContext;
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::errors::storage_error,
    schema::relation_alteration::{relocation::ensure_name_available, RelationAlterNames},
    SQLError,
};
use uqa_storage::CatalogIndexRow;

pub struct PreparedIndexRelocations {
    rows: Vec<(CatalogIndexRow, CatalogIndexRow)>,
}

impl PreparedIndexRelocations {
    /// Every identity and destination lock was retained before the owning relation moved. Publication must not reload the partly moved catalog.
    pub fn publish(self, publication: &dyn IndexRegistryPublication) -> Result<(), SQLError> {
        for (current, renamed) in self.rows {
            IndexRegistryChange::rename_with_publication(publication, &current, renamed)
                .map_err(|error| storage_error("move index namespace", &error))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

pub fn prepare_table_indexes(
    context: &IndexRegistryContext<'_>,
    creation: &RelationCreationContext<'_>,
    names: &dyn RelationAlterNames,
    source: &RelationIdentity,
    target: &RelationIdentity,
) -> Result<PreparedIndexRelocations, SQLError> {
    let catalog = context.identities.catalog.current_catalog_snapshot();
    let from = source.qualified_name();
    let to = target.qualified_name();
    let mut identities = Vec::new();
    for row in catalog.catalog_indexes().filter(|row| {
        row.relation.schema == source.schema && (row.table_name == from || row.table_name == to)
    }) {
        let identity = crate::catalog::index::index_definition(row)
            .map_err(|error| storage_error("move index namespace", &error))?
            .catalog
            .ok_or_else(|| SQLError::Internal("moved index has no catalog identity".into()))?
            .identity;
        identities.push(identity);
    }
    identities.sort_by_key(|identity| identity.oid);
    let mut rows = Vec::new();
    for identity in identities {
        let Some(current) = super::registry::binding::index_identity(
            context,
            identity.object_id,
            RelationLockMode::AccessExclusive,
        )
        .map_err(|error| storage_error("move index namespace", &error))?
        else {
            continue;
        };
        if current.relation.schema == target.schema {
            continue;
        }
        let destination = RelationIdentity::new(&target.schema, &current.relation.name);
        ensure_name_available(names, &destination)?;
        creation.reserve_name(&destination.qualified_name())?;
        let mut renamed = current.clone();
        renamed.relation = destination;
        renamed.table_name.clone_from(&to);
        rows.push((current, renamed));
    }
    Ok(PreparedIndexRelocations { rows })
}
