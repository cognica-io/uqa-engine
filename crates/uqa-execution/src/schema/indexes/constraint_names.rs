//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Key structure belongs to the table declaration; each key's current name belongs to its owned index.

use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_sql::ast::{TableConstraintSet, TableKeyConstraint};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult, TableSchema};

pub const REGISTRY_VERSION: &str = "sql_index_registry_version";

/// One command snapshot's owned index names, shared by every table decoder in that restore.
pub struct KeyConstraintNames {
    names: Option<BTreeMap<[u8; 16], KeyName>>,
}

struct KeyName {
    table: RelationIdentity,
    table_object_id: [u8; 16],
    name: String,
}

/// A structural DDL candidate must not restore an independently renamed index's old name.
pub(crate) fn rebind_current_key_names<'a>(
    candidate: impl Iterator<Item = &'a mut TableKeyConstraint>,
    current: &crate::catalog::CatalogTableSnapshot,
) {
    for key in candidate {
        let Some(identity) = key.catalog_identity else {
            continue;
        };
        if let Some(retained) = current
            .keys
            .iter()
            .chain(&current.hierarchy.partition_inherited_key_constraints)
            .find(|other| {
                other
                    .catalog_identity
                    .is_some_and(|other| other.object_id == identity.object_id)
            })
        {
            key.name.clone_from(&retained.name);
        }
    }
}

impl KeyConstraintNames {
    pub fn load(catalog: &dyn CatalogFacade) -> StorageBackendResult<Self> {
        match catalog.get_metadata(REGISTRY_VERSION)?.as_deref() {
            None | Some("1") => return Ok(Self { names: None }),
            Some("2") => {}
            _ => return Err(invalid("unsupported index registry format")),
        }
        let mut names = BTreeMap::new();
        for row in catalog.load_catalog_indexes()? {
            let definition = crate::catalog::index::index_definition(&row)?;
            let Some(owner) = definition.relationships.owning_constraint else {
                continue;
            };
            let identity = definition
                .catalog
                .ok_or_else(|| invalid("owned index has no catalog identity"))?;
            let table = RelationIdentity::from_legacy_name(&row.table_name).map_err(invalid)?;
            if row.relation.schema != table.schema
                || names
                    .insert(
                        owner,
                        KeyName {
                            table,
                            table_object_id: identity.table_object_id,
                            name: row.relation.name,
                        },
                    )
                    .is_some()
            {
                return Err(invalid("invalid or duplicate key constraint name owner"));
            }
        }
        Ok(Self { names: Some(names) })
    }

    pub fn decode(&self, schema: &TableSchema) -> StorageBackendResult<TableConstraintSet> {
        let mut constraints: TableConstraintSet = if schema.constraints_json.is_empty() {
            TableConstraintSet::default()
        } else {
            serde_json::from_str(&schema.constraints_json)?
        };
        let Some(names) = &self.names else {
            return Ok(constraints);
        };
        for key in constraints.key_constraints.iter_mut().chain(
            constraints
                .hierarchy
                .partition_inherited_key_constraints
                .iter_mut(),
        ) {
            if key.name.is_some() {
                return Err(invalid("stored key name must be owned by its index"));
            }
            let owner = key
                .catalog_identity
                .ok_or_else(|| invalid("key name has no constraint identity"))?;
            let name = names
                .get(&owner.object_id)
                .ok_or_else(|| invalid("key constraint has no owned index name"))?;
            if name.table != schema.relation || name.table_object_id != schema.object_id {
                return Err(invalid("key constraint name belongs to a different table"));
            }
            key.name = Some(name.name.clone());
        }
        Ok(constraints)
    }

    /// Registry-only changes must also refresh retained table generations whose structural record did not change.
    pub(super) fn project(
        &self,
        catalog: &crate::catalog::CatalogReadView,
    ) -> StorageBackendResult<(
        crate::catalog::CatalogReadView,
        std::collections::BTreeSet<RelationIdentity>,
    )> {
        let mut snapshot = catalog.snapshot().clone();
        let mut changed = std::collections::BTreeSet::new();
        if let Some(names) = &self.names {
            for (relation, table) in &mut snapshot.tables {
                if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                    continue;
                }
                for key in std::sync::Arc::make_mut(&mut table.keys).iter_mut().chain(
                    std::sync::Arc::make_mut(&mut table.hierarchy)
                        .partition_inherited_key_constraints
                        .iter_mut(),
                ) {
                    let owner = key
                        .catalog_identity
                        .ok_or_else(|| invalid("key name has no constraint identity"))?;
                    let name = names
                        .get(&owner.object_id)
                        .ok_or_else(|| invalid("key constraint has no owned index name"))?;
                    if &name.table != relation || name.table_object_id != table.object_id {
                        return Err(invalid("key constraint name belongs to a different table"));
                    }
                    if key.name.as_ref() != Some(&name.name) {
                        key.name = Some(name.name.clone());
                        changed.insert(relation.clone());
                    }
                }
            }
        }
        Ok((crate::catalog::CatalogReadView::new(snapshot), changed))
    }

    pub fn encode(&self, constraints: &TableConstraintSet) -> StorageBackendResult<String> {
        if self.names.is_some() {
            encode(constraints)
        } else {
            serde_json::to_string(constraints).map_err(Into::into)
        }
    }
}

/// Persist key structure without a second mutable copy of the owned index's name.
pub fn encode(constraints: &TableConstraintSet) -> StorageBackendResult<String> {
    let mut structural = constraints.clone();
    for key in structural.key_constraints.iter_mut().chain(
        structural
            .hierarchy
            .partition_inherited_key_constraints
            .iter_mut(),
    ) {
        clear_name(key)?;
    }
    serde_json::to_string(&structural).map_err(Into::into)
}

fn clear_name(key: &mut TableKeyConstraint) -> StorageBackendResult<()> {
    if key.catalog_identity.is_none() {
        return Err(invalid("stored key structure has no constraint identity"));
    }
    key.name = None;
    Ok(())
}

fn invalid(message: impl ToString) -> StorageBackendError {
    StorageBackendError::Other(message.to_string())
}

#[cfg(test)]
mod tests;
