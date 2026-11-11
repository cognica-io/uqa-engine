//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Key structure belongs to the table declaration; each key's current name belongs to its owned index.

use uqa_core::{
    memory::{MemoryBudget, MemoryReservation, OwnedMap, Produced, ProductionControl},
    CancellationToken, RelationIdentity,
};

mod input;
pub use input::KeyConstraintNameInput;
use uqa_sql::ast::{TableConstraintSet, TableKeyConstraint};
use uqa_sql::catalog::table_definition::{StoredKeyLocation, StoredTableConstraints};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult, TableSchema};

pub const REGISTRY_VERSION: &str = "sql_index_registry_version";

/// One command snapshot's owned index names, shared by every table decoder in that restore.
pub struct KeyConstraintNames {
    names: Option<OwnedMap<[u8; 16], KeyName>>,
}

struct KeyName {
    table: RelationIdentity,
    table_object_id: [u8; 16],
    name: String,
    _memory: Option<MemoryReservation>,
}

/// A structural DDL candidate must not restore an independently renamed index's old name.
pub(crate) fn rebind_current_key_names<'a>(
    candidate: impl Iterator<Item = &'a mut TableKeyConstraint>,
    current: &crate::catalog::CatalogTableSnapshot,
) {
    rebind_key_names(
        candidate,
        current
            .keys
            .iter()
            .chain(&current.hierarchy.partition_inherited_key_constraints),
    );
}

pub(crate) fn rebind_key_names<'a, 'b>(
    candidate: impl Iterator<Item = &'a mut TableKeyConstraint>,
    current: impl Iterator<Item = &'b TableKeyConstraint> + Clone,
) {
    for key in candidate {
        let Some(identity) = key.catalog_identity else {
            continue;
        };
        if let Some(retained) = current.clone().find(|other| {
            other
                .catalog_identity
                .is_some_and(|other| other.object_id == identity.object_id)
        }) {
            key.name.clone_from(&retained.name);
        }
    }
}

impl KeyConstraintNames {
    pub fn load(catalog: &dyn CatalogFacade) -> StorageBackendResult<Self> {
        if !index_owns_names(catalog)? {
            return Ok(Self { names: None });
        }
        let mut result = Self {
            names: Some(OwnedMap::new()),
        };
        for row in catalog.load_catalog_indexes()? {
            let definition = crate::catalog::index::index_definition(&row)?;
            let Some(owner) = definition.relationships.owning_constraint else {
                continue;
            };
            let identity = definition
                .catalog
                .ok_or_else(|| invalid("owned index has no catalog identity"))?;
            let table = RelationIdentity::from_legacy_name(&row.table_name).map_err(invalid)?;
            result.insert(
                owner,
                &row.relation.schema,
                KeyName {
                    table,
                    table_object_id: identity.table_object_id,
                    name: row.relation.name,
                    _memory: None,
                },
                &ProductionControl::uncontrolled(),
            )?;
        }
        Ok(result)
    }

    fn insert(
        &mut self,
        owner: [u8; 16],
        index_schema: &str,
        name: KeyName,
        control: &ProductionControl<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        if index_schema != name.table.schema {
            return Err(invalid("invalid or duplicate key constraint name owner"));
        }
        let names = self.names.as_mut().expect("index-owned key names");
        let previous = if let Some(memory) = control.budget() {
            names.insert_budgeted(owner, name, memory)?
        } else {
            names.insert(owner, name)
        };
        if previous.is_some() {
            return Err(invalid("invalid or duplicate key constraint name owner"));
        }
        Ok(())
    }

    pub fn decode(&self, schema: &TableSchema) -> StorageBackendResult<TableConstraintSet> {
        let constraints = if schema.constraints_json.is_empty() {
            TableConstraintSet::default()
        } else {
            serde_json::from_str(&schema.constraints_json)?
        };
        let control = ProductionControl::uncontrolled();
        Ok(self
            .restore_names(schema, control.finish(constraints, None)?, &control)?
            .into_uncontrolled()
            .expect("ordinary table input retains no allowance"))
    }

    /// Retain native table metadata and current index-owned key names under the same original allowance. Names are copied only after their stored constraint and table identities match.
    pub fn decode_with_control(
        &self,
        schema: &TableSchema,
        memory: &MemoryBudget,
        original: &CancellationToken,
        invoking: &CancellationToken,
    ) -> StorageBackendResult<Produced<TableConstraintSet>> {
        let control = ProductionControl::new(memory, original, invoking);
        let constraints = if schema.constraints_json.is_empty() {
            control.finish(TableConstraintSet::default(), control.empty_reservation())?
        } else {
            uqa_sql::ast::read_stored_table_constraints_json(
                &schema.constraints_json,
                memory,
                original,
                invoking,
            )
            .map_err(input::sql_error)?
        };
        self.restore_names(schema, constraints, &control)
    }

    /// Keep independently admitted table fields through current-name restoration. The complete unpublished declaration remains available for borrowed semantic validation before native publication.
    pub fn decode_stored_with_control(
        &self,
        schema: &TableSchema,
        memory: &MemoryBudget,
        original: &CancellationToken,
        invoking: &CancellationToken,
    ) -> StorageBackendResult<StoredTableConstraints> {
        self.decode_stored_fields_with_control(
            &schema.relation,
            schema.object_id,
            &schema.constraints_json,
            memory,
            original,
            invoking,
        )
    }

    /// Decode independently retained fields without rebuilding the surrounding stored table. Current index names remain bound to the exact relation and object identity.
    pub fn decode_stored_fields_with_control(
        &self,
        relation: &RelationIdentity,
        table_object_id: [u8; 16],
        text: &str,
        memory: &MemoryBudget,
        original: &CancellationToken,
        invoking: &CancellationToken,
    ) -> StorageBackendResult<StoredTableConstraints> {
        let text = if text.is_empty() { "{}" } else { text };
        let mut constraints =
            StoredTableConstraints::read_stored_json(text, memory, original, invoking)
                .map_err(input::sql_error)?;
        let control = ProductionControl::new(memory, original, invoking);
        if let Some(names) = &self.names {
            for inherited in [false, true] {
                let count = if inherited {
                    constraints
                        .hierarchy
                        .partition_inherited_key_constraints
                        .len()
                } else {
                    constraints.key_constraints.len()
                };
                for index in 0..count {
                    let (key, location) = if inherited {
                        (
                            &constraints.hierarchy.partition_inherited_key_constraints[index],
                            StoredKeyLocation::Inherited(index),
                        )
                    } else {
                        (
                            &constraints.key_constraints[index],
                            StoredKeyLocation::Declared(index),
                        )
                    };
                    let name = restored_key_name(names, relation, table_object_id, key, &control)?;
                    constraints
                        .replace_key_name(location, name)
                        .map_err(input::sql_error)?;
                }
            }
        }
        Ok(constraints)
    }

    fn restore_names(
        &self,
        schema: &TableSchema,
        constraints: Produced<TableConstraintSet>,
        control: &ProductionControl<'_>,
    ) -> StorageBackendResult<Produced<TableConstraintSet>> {
        let mut memory = control.empty_reservation();
        let mut constraints = {
            let (value, owned) = constraints.into_parts();
            memory = control.combine(memory, owned);
            value
        };
        if let Some(names) = &self.names {
            for key in constraints
                .key_constraints
                .iter_mut()
                .chain(&mut constraints.hierarchy.partition_inherited_key_constraints)
            {
                let (name, owned) =
                    restored_key_name(names, &schema.relation, schema.object_id, key, control)?
                        .into_parts();
                key.name = Some(name);
                memory = control.combine(memory, owned);
            }
        }
        Ok(control.finish(constraints, memory)?)
    }

    /// Registry-only changes must also refresh retained table generations whose structural record did not change.
    pub(super) fn project(
        &self,
        catalog: &crate::catalog::CatalogReadView,
    ) -> StorageBackendResult<(
        crate::catalog::CatalogReadView,
        std::collections::BTreeSet<RelationIdentity>,
    )> {
        let mut snapshot = None;
        let mut changed = std::collections::BTreeSet::new();
        if let Some(names) = &self.names {
            for (relation, table) in &catalog.snapshot().tables {
                if table.persistence == uqa_sql::ast::RelationPersistence::Temporary {
                    continue;
                }
                let keys = project_keys(names, relation, table.object_id, &table.keys)?;
                let inherited = project_keys(
                    names,
                    relation,
                    table.object_id,
                    &table.hierarchy.partition_inherited_key_constraints,
                )?;
                if keys.is_none() && inherited.is_none() {
                    continue;
                }
                let candidate = snapshot.get_or_insert_with(|| catalog.snapshot().clone());
                let table = candidate.tables.get_mut(relation).expect("retained table");
                if let Some(keys) = keys {
                    table.keys = keys.into();
                }
                if let Some(inherited) = inherited {
                    let mut hierarchy = (*table.hierarchy).clone();
                    hierarchy.partition_inherited_key_constraints = inherited;
                    table.hierarchy = hierarchy.into();
                }
                changed.insert(relation.clone());
            }
        }
        Ok((
            snapshot.map_or_else(|| catalog.clone(), crate::catalog::CatalogReadView::new),
            changed,
        ))
    }

    pub fn encode(&self, constraints: &TableConstraintSet) -> StorageBackendResult<String> {
        if self.names.is_some() {
            encode(constraints)
        } else {
            serde_json::to_string(constraints).map_err(Into::into)
        }
    }
}

fn restored_key_name(
    names: &OwnedMap<[u8; 16], KeyName>,
    relation: &RelationIdentity,
    table_object_id: [u8; 16],
    key: &TableKeyConstraint,
    control: &ProductionControl<'_>,
) -> StorageBackendResult<Produced<String>> {
    control.check()?;
    if key.name.is_some() {
        return Err(invalid("stored key name must be owned by its index"));
    }
    let name = current_name(names, relation, table_object_id, key)?;
    Ok(control.copy_text(name)?)
}

/// Validate every owner even when its name is unchanged, and copy only a changed key collection.
fn project_keys(
    names: &OwnedMap<[u8; 16], KeyName>,
    relation: &RelationIdentity,
    table_object_id: [u8; 16],
    keys: &[TableKeyConstraint],
) -> StorageBackendResult<Option<Vec<TableKeyConstraint>>> {
    let mut projected = None;
    for (index, key) in keys.iter().enumerate() {
        let name = current_name(names, relation, table_object_id, key)?;
        if key.name.as_deref() != Some(name) {
            projected.get_or_insert_with(|| keys.to_vec())[index].name = Some(name.to_owned());
        }
    }
    Ok(projected)
}

fn current_name<'a>(
    names: &'a OwnedMap<[u8; 16], KeyName>,
    relation: &RelationIdentity,
    table_object_id: [u8; 16],
    key: &TableKeyConstraint,
) -> StorageBackendResult<&'a str> {
    let owner = key
        .catalog_identity
        .ok_or_else(|| invalid("key name has no constraint identity"))?;
    let name = names
        .get(&owner.object_id)
        .ok_or_else(|| invalid("key constraint has no owned index name"))?;
    if &name.table != relation || name.table_object_id != table_object_id {
        return Err(invalid("key constraint name belongs to a different table"));
    }
    Ok(&name.name)
}

fn index_owns_names(catalog: &dyn CatalogFacade) -> StorageBackendResult<bool> {
    registry_owns_names(catalog.get_metadata(REGISTRY_VERSION)?.as_deref())
}

fn registry_owns_names(version: Option<&str>) -> StorageBackendResult<bool> {
    match version {
        None | Some("1") => Ok(false),
        Some("2") => Ok(true),
        _ => Err(invalid("unsupported index registry format")),
    }
}

/// Initial catalog migrations may save tables before their key names have moved to owned indexes.
pub fn encode_for_catalog(
    catalog: &dyn CatalogFacade,
    constraints: &TableConstraintSet,
) -> StorageBackendResult<String> {
    if index_owns_names(catalog)? {
        encode(constraints)
    } else {
        serde_json::to_string(constraints).map_err(Into::into)
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
