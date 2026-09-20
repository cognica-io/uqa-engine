//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish explicit index trees through the same registry as constraint-owned indexes.

use super::{
    difference, index_definition, invalid, partitions, same_row, schema, validation, BTreeMap,
    BTreeSet, CatalogIndexRow, IndexRegistryChange, IndexRegistryContext, RelationIdentity,
    StorageBackendError, StorageBackendResult,
};

pub fn register(
    context: &IndexRegistryContext<'_>,
    row: CatalogIndexRow,
) -> StorageBackendResult<()> {
    let catalog = context.identities.catalog.current_catalog_snapshot();
    let previous = &catalog.snapshot().definitions.catalog_indexes;
    if let Some(old) = previous.get(&row.relation) {
        ensure_independent(old)?;
    }
    if !index_definition(&row)?.relationships.is_empty() {
        return Err(invalid(
            "explicit registration cannot replace index ownership or ancestry",
        ));
    }
    let mut rows = previous.as_ref().clone();
    if let Some(old) = previous.get(&row.relation) {
        if !same_row(old, &row) {
            let old_tree = descendants(previous, &row.relation)?;
            require_no_references(&catalog, &old_tree)?;
            for child in old_tree.iter().skip(1) {
                if index_definition(child)?
                    .relationships
                    .owning_constraint
                    .is_some()
                {
                    return Err(dependency(
                        "an owned partition index prevents direct replacement",
                    ));
                }
                rows.remove(&child.relation);
            }
        }
    }
    let table = RelationIdentity::from_legacy_name(&row.table_name).map_err(invalid)?;
    rows.insert(row.relation.clone(), row);
    let mut allocator = context
        .identities
        .allocator(crate::catalog::identity::allocate_catalog_object_id);
    partitions::materialize(&catalog, &mut rows, &mut allocator)?;
    validation::validate(&catalog, &rows)?;
    super::names::reserve_new_names(context, previous, &rows)?;
    super::recheck::validate(context, &catalog, &catalog, &rows, &table)?;
    difference(previous, &rows)?.publish(context)
}

pub fn descendants(
    rows: &BTreeMap<RelationIdentity, CatalogIndexRow>,
    root: &RelationIdentity,
) -> StorageBackendResult<Vec<CatalogIndexRow>> {
    let Some(root) = rows.get(root) else {
        return Ok(Vec::new());
    };
    let mut result = Vec::new();
    let mut pending = std::collections::VecDeque::from([root.clone()]);
    let mut visited = BTreeSet::new();
    while let Some(row) = pending.pop_front() {
        let id = index_definition(&row)?
            .catalog
            .ok_or_else(|| invalid("index has no identity"))?
            .identity
            .object_id;
        if !visited.insert(id) {
            return Err(invalid("cyclic index ancestry"));
        }
        for child in rows.values() {
            if index_definition(child)?.relationships.parent_index == Some(id) {
                pending.push_back(child.clone());
            }
        }
        result.push(row);
    }
    Ok(result)
}

pub fn remove(
    context: &IndexRegistryContext<'_>,
    relation: &RelationIdentity,
    cascade: bool,
) -> StorageBackendResult<Option<CatalogIndexRow>> {
    let catalog = context.identities.catalog.current_catalog_snapshot();
    let previous = &catalog.snapshot().definitions.catalog_indexes;
    let Some(root) = previous.get(relation) else {
        return Ok(None);
    };
    ensure_independent(root)?;
    let removed = descendants(previous, relation)?;
    require_no_references(&catalog, &removed)?;
    if !cascade {
        for child in removed.iter().skip(1) {
            if index_definition(child)?
                .relationships
                .owning_constraint
                .is_some()
            {
                return Err(dependency(
                    "an owned partition index requires cascading removal",
                ));
            }
        }
    }
    let mut change = IndexRegistryChange {
        removals: removed,
        ..Default::default()
    };
    let mut owners = BTreeMap::<String, BTreeSet<[u8; 16]>>::new();
    for row in &change.removals {
        if let Some(owner) = index_definition(row)?.relationships.owning_constraint {
            owners
                .entry(row.table_name.clone())
                .or_default()
                .insert(owner);
        }
    }
    for (table, owners) in owners {
        let state = context
            .tables
            .table_state(&table)?
            .ok_or_else(|| invalid("owned child index has no table"))?;
        let mut columns = state.columns();
        let mut constraints = state.constraints();
        schema::remove_keys(&mut columns, &mut constraints, &owners);
        change.schema.push(schema::OwnerChange {
            relation: RelationIdentity::from_legacy_name(&table).map_err(invalid)?,
            object_id: state.object_id(),
            columns,
            constraints,
        });
    }
    change.publish(context)?;
    Ok(Some(root.clone()))
}

fn ensure_independent(row: &CatalogIndexRow) -> StorageBackendResult<()> {
    let definition = index_definition(row)?;
    if definition.relationships.owning_constraint.is_some()
        || definition.relationships.parent_index.is_some()
    {
        return Err(StorageBackendError::backend("index dependency", uqa_sql::SQLError::Routine {
            sqlstate: "2BP01".into(), message: format!("cannot replace or remove index {} because a constraint or parent index requires it", row.relation.qualified_name()),
        }));
    }
    Ok(())
}

fn require_no_references(
    catalog: &crate::catalog::CatalogReadView,
    rows: &[CatalogIndexRow],
) -> StorageBackendResult<()> {
    let identities = rows
        .iter()
        .map(|row| {
            Ok(index_definition(row)?
                .catalog
                .ok_or_else(|| invalid("index has no identity"))?
                .identity
                .object_id)
        })
        .collect::<StorageBackendResult<BTreeSet<_>>>()?;
    for table in catalog.snapshot().tables.values() {
        if table.foreign_keys.iter().any(|key| {
            key.referenced_index
                .is_some_and(|id| identities.contains(&id))
        }) || table
            .columns
            .iter()
            .filter_map(|column| column.references.as_ref())
            .any(|key| {
                key.referenced_index
                    .is_some_and(|id| identities.contains(&id))
            })
        {
            return Err(dependency(
                "cannot remove or replace an index required by a foreign key",
            ));
        }
    }
    Ok(())
}

fn dependency(message: &str) -> StorageBackendError {
    StorageBackendError::backend(
        "index dependency",
        uqa_sql::SQLError::Routine {
            sqlstate: "2BP01".into(),
            message: message.into(),
        },
    )
}
