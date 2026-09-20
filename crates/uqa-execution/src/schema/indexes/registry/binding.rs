//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coordinate direct index registry operations with SQL definition locks and refreshed identities.

use super::{
    index_definition, invalid, BTreeSet, CatalogIndexRow, IndexRegistryContext, RelationIdentity,
    StorageBackendError, StorageBackendResult,
};
use crate::row_locks::{
    binding::{bind_relation, lock_relation_identity, RelationBinding},
    RelationLockMode,
};
use uqa_sql::SQLError;

pub fn table(
    context: &IndexRegistryContext<'_>,
    requested: &str,
    mode: RelationLockMode,
) -> StorageBackendResult<RelationIdentity> {
    let bound = bind_relation(
        context.locks,
        mode,
        false,
        || {
            let catalog = context.identities.catalog.current_catalog_snapshot();
            let resolution = context.identities.session.relation_name_resolution();
            let name = catalog
                .table_name(&resolution, requested)?
                .ok_or_else(|| SQLError::UnknownTable(requested.into()))?;
            let relation = RelationIdentity::from_legacy_name(&name).map_err(SQLError::Internal)?;
            let identity = catalog
                .snapshot()
                .tables
                .get(&relation)
                .ok_or_else(|| SQLError::UnknownTable(name.clone()))?
                .object_id;
            Ok(Some(RelationBinding {
                name,
                object_id: Some(identity),
                value: relation,
            }))
        },
        |_| Ok(()),
    )
    .map_err(storage_error)?
    .ok_or_else(|| invalid("required indexed table disappeared"))?;
    lock_partition_tree(context, &bound.name, mode)?;
    Ok(bound.value)
}

pub fn registration(
    context: &IndexRegistryContext<'_>,
    requested: &str,
    target: &str,
) -> StorageBackendResult<(RelationIdentity, RelationIdentity)> {
    let table = table(context, target, RelationLockMode::Share)?;
    let (schema, name) = RelationIdentity::parse_reference(requested).map_err(invalid)?;
    if schema.is_some_and(|schema| schema != table.schema) {
        return Err(invalid("index and table must belong to the same schema"));
    }
    let relation = RelationIdentity::new(&table.schema, name);
    removal(context, &relation)?;
    Ok((relation, table))
}

pub fn removal(
    context: &IndexRegistryContext<'_>,
    relation: &RelationIdentity,
) -> StorageBackendResult<Option<CatalogIndexRow>> {
    let bound = bind_relation(
        context.locks,
        RelationLockMode::AccessExclusive,
        false,
        || {
            let catalog = context.identities.catalog.current_catalog_snapshot();
            let Some(row) = catalog.snapshot().definitions.catalog_indexes.get(relation) else {
                return Ok(None);
            };
            let definition = index_definition(row).map_err(|error| {
                uqa_sql::catalog::errors::storage_error("index binding", &error)
            })?;
            Ok(Some(RelationBinding {
                name: row.table_name.clone(),
                object_id: definition
                    .catalog
                    .map(|identity| identity.identity.object_id),
                value: row.clone(),
            }))
        },
        |_| Ok(()),
    )
    .map_err(storage_error)?;
    if let Some(bound) = &bound {
        lock_partition_tree(context, &bound.name, RelationLockMode::AccessExclusive)?;
    }
    Ok(bound.map(|binding| binding.value))
}

fn lock_partition_tree(
    context: &IndexRegistryContext<'_>,
    root: &str,
    mode: RelationLockMode,
) -> StorageBackendResult<()> {
    let mut pending = std::collections::VecDeque::from([root.to_string()]);
    let mut seen = BTreeSet::new();
    while let Some(parent) = pending.pop_front() {
        if !seen.insert(parent.clone()) {
            return Err(invalid("cyclic partition definition"));
        }
        let catalog = context.identities.catalog.current_catalog_snapshot();
        let children = catalog
            .snapshot()
            .tables
            .iter()
            .filter(|(_, table)| {
                table.hierarchy.is_partition() && table.hierarchy.parents.first() == Some(&parent)
            })
            .map(|(name, table)| (name.qualified_name(), table.object_id))
            .collect::<Vec<_>>();
        for (name, identity) in children {
            if let Some(current) = lock_relation_identity(
                context.lock_catalog,
                context.locks,
                name,
                identity,
                mode,
                false,
            )
            .map_err(storage_error)?
            {
                pending.push_back(current);
            }
        }
    }
    Ok(())
}

fn storage_error(error: SQLError) -> StorageBackendError {
    StorageBackendError::backend("index definition binding", error)
}
