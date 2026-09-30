//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind every owned key index to its independent constraint and physical identities.

use super::{
    index_definition, invalid, metadata_error, same_row, BTreeMap, CatalogIndexRow,
    CatalogObjectAllocator, CatalogOidClass, ColumnDef, IndexCatalogIdentity, IndexDefinition,
    IndexKey, IndexRegistryChange, RelationIdentity, StorageBackendResult, TableConstraintSet,
};

pub(in crate::schema::indexes) fn prepare(
    catalog: &crate::catalog::CatalogReadView,
    table: &str,
    table_object_id: [u8; 16],
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
    allocator: &mut dyn CatalogObjectAllocator,
) -> StorageBackendResult<IndexRegistryChange> {
    let relation = RelationIdentity::from_legacy_name(table).map_err(invalid)?;
    let previous = &catalog.snapshot().definitions.catalog_indexes;
    let mut owned = BTreeMap::new();
    for row in previous.values().filter(|row| row.table_name == table) {
        let definition = index_definition(row)?;
        if let Some(owner) = definition.relationships.owning_constraint {
            if owned.insert(owner, (row, definition)).is_some() {
                return Err(invalid("constraint owns more than one index"));
            }
        }
    }
    let mut change = IndexRegistryChange::default();
    for key in &constraints.key_constraints {
        let owner = key
            .catalog_identity
            .ok_or_else(|| invalid("index owner has no constraint identity"))?;
        let name = key
            .name
            .as_ref()
            .ok_or_else(|| invalid("index owner has no constraint name"))?;
        let name = RelationIdentity::new(&relation.schema, name);
        let mut definition = IndexDefinition::for_constraint(key, columns).map_err(invalid)?;
        let previous_owned = owned.remove(&owner.object_id);
        let retained = previous_owned
            .as_ref()
            .map(|(_, definition)| {
                definition
                    .catalog
                    .clone()
                    .ok_or_else(|| invalid("owned index has no catalog identity"))
            })
            .transpose()?;
        if let Some(existing) = previous.get(&name) {
            if previous_owned
                .as_ref()
                .is_none_or(|(row, _)| row.relation != existing.relation)
            {
                return Err(invalid(format!(
                    "constraint index `{}` conflicts with another relation",
                    name.qualified_name()
                )));
            }
        }
        let identity = if let Some(identity) = retained {
            identity.validate(table_object_id).map_err(invalid)?;
            allocator
                .include_catalog_identity(
                    &previous_owned.as_ref().expect("retained index").0.relation,
                    CatalogOidClass::Relation,
                    identity.identity,
                )
                .map_err(metadata_error)?;
            definition.relationships.parent_index = previous_owned
                .as_ref()
                .and_then(|(_, definition)| definition.relationships.parent_index);
            identity
        } else {
            new_key_index_identity(key, &name, table_object_id, allocator)?
        };
        definition
            .relationships
            .validate(identity.identity.object_id)
            .map_err(invalid)?;
        definition.catalog = Some(identity);
        let row = CatalogIndexRow {
            relation: name,
            table_name: table.to_string(),
            index_type: if key.without_overlaps {
                "gist"
            } else {
                "btree"
            }
            .into(),
            columns_json: serde_json::to_string(
                &key.columns
                    .iter()
                    .cloned()
                    .map(IndexKey::Column)
                    .collect::<Vec<_>>(),
            )?,
            parameters_json: "{}".into(),
            definition_json: Some(serde_json::to_string(&definition)?),
        };
        if let Some((old, _)) = previous_owned {
            if old.relation != row.relation {
                change.removals.push(old.clone());
            }
            if same_row(old, &row) {
                continue;
            }
        }
        change.upserts.push(row);
    }
    change
        .removals
        .extend(owned.into_values().map(|(row, _)| row.clone()));
    Ok(change)
}

/// A key whose index is not registered yet binds the index identity reserved with the key, as `index_create` precedes `index_constraint_create`; a key recorded before that reservation allocates one now.
fn new_key_index_identity(
    key: &uqa_sql::ast::TableKeyConstraint,
    name: &RelationIdentity,
    table_object_id: [u8; 16],
    allocator: &mut dyn CatalogObjectAllocator,
) -> StorageBackendResult<IndexCatalogIdentity> {
    match key.index_identity {
        Some(reserved) => {
            allocator
                .include_catalog_identity(name, CatalogOidClass::Relation, reserved)
                .map_err(metadata_error)?;
            IndexCatalogIdentity::reserved(table_object_id, reserved).map_err(metadata_error)
        }
        None => IndexCatalogIdentity::allocate(table_object_id, allocator).map_err(metadata_error),
    }
}
