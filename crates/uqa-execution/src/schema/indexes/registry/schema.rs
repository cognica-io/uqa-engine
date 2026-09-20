//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare descendant key declarations before any catalog or index writes.

use super::{
    index_definition, invalid, metadata_error, partitions, BTreeSet, CatalogObjectAllocator,
    ColumnDef, RelationIdentity, StorageBackendError, StorageBackendResult, TableConstraintSet,
};
use crate::catalog::{CatalogReadSnapshot, CatalogReadView};
use crate::schema::publication::{TableSchemaCatalog, TableSchemaState};
use uqa_sql::schema::constraint_metadata::materialize_constraint_metadata_with_names;

pub(in crate::schema::indexes) struct OwnerChange {
    pub relation: RelationIdentity,
    pub object_id: [u8; 16],
    pub columns: Vec<ColumnDef>,
    pub constraints: TableConstraintSet,
}

impl OwnerChange {
    pub fn current<'a>(
        &self,
        catalog: &'a dyn TableSchemaCatalog,
    ) -> StorageBackendResult<Box<dyn TableSchemaState + 'a>> {
        let state = catalog
            .table_state(&self.relation.qualified_name())?
            .ok_or_else(|| invalid("index owner disappeared during preparation"))?;
        if state.object_id() != self.object_id {
            return Err(invalid("index owner changed during preparation"));
        }
        Ok(state)
    }
}

pub(super) fn refresh_names(
    catalog: &CatalogReadView,
    root: &RelationIdentity,
    constraints: &mut TableConstraintSet,
    changes: &mut [OwnerChange],
) -> StorageBackendResult<()> {
    for (relation, constraints) in std::iter::once((root, constraints)).chain(
        changes
            .iter_mut()
            .map(|change| (&change.relation, &mut change.constraints)),
    ) {
        let table = catalog
            .snapshot()
            .tables
            .get(relation)
            .ok_or_else(|| invalid("index owner disappeared during name refresh"))?;
        crate::schema::indexes::constraint_names::rebind_current_key_names(
            constraints.key_constraints.iter_mut().chain(
                constraints
                    .hierarchy
                    .partition_inherited_key_constraints
                    .iter_mut(),
            ),
            table,
        );
    }
    Ok(())
}

pub(super) fn refreshed_candidate(
    catalog: &CatalogReadView,
    root: &RelationIdentity,
    columns: &[ColumnDef],
    constraints: &mut TableConstraintSet,
    changes: &mut [OwnerChange],
) -> StorageBackendResult<CatalogReadView> {
    refresh_names(catalog, root, constraints, changes)?;
    let mut candidate = catalog.snapshot().clone();
    replace(&mut candidate, root, columns, constraints)?;
    for change in changes {
        replace(
            &mut candidate,
            &change.relation,
            &change.columns,
            &change.constraints,
        )?;
    }
    Ok(CatalogReadView::new(candidate))
}

pub(in crate::schema::indexes) fn replace(
    snapshot: &mut CatalogReadSnapshot,
    relation: &RelationIdentity,
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
) -> StorageBackendResult<()> {
    let table = snapshot
        .tables
        .get_mut(relation)
        .ok_or_else(|| invalid("index owner disappeared"))?;
    table.columns = columns.to_vec().into();
    table.keys = constraints.key_constraints.clone().into();
    table.checks = constraints.checks.clone().into();
    table.foreign_keys = constraints.foreign_keys.clone().into();
    table.hierarchy = constraints.hierarchy.clone().into();
    Ok(())
}

pub(in crate::schema::indexes) fn prepare_descendants(
    original: &CatalogReadView,
    candidate: &mut CatalogReadSnapshot,
    root: &RelationIdentity,
    allocator: &mut dyn CatalogObjectAllocator,
    lookup: impl Fn(
        &RelationIdentity,
    ) -> StorageBackendResult<(Vec<ColumnDef>, TableConstraintSet, [u8; 16])>,
) -> StorageBackendResult<Vec<OwnerChange>> {
    let mut pending = std::collections::VecDeque::from([root.clone()]);
    let mut visited = BTreeSet::new();
    let mut changes = Vec::new();
    while let Some(parent) = pending.pop_front() {
        if !visited.insert(parent.clone()) {
            return Err(invalid("cyclic partition index ancestry"));
        }
        let parent_keys = candidate.tables[&parent].keys.clone();
        let removed_parent_indexes = removed_parent_indexes(original, &parent, &parent_keys)?;
        let children = candidate
            .tables
            .iter()
            .filter(|(_, table)| {
                table.hierarchy.is_partition()
                    && table.hierarchy.parents.first() == Some(&parent.qualified_name())
            })
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        for child in children {
            let (mut columns, mut constraints, object_id) = lookup(&child)?;
            let before = constraints.clone();
            let removed_owners = original
                .snapshot()
                .definitions
                .catalog_indexes
                .values()
                .filter(|row| row.table_name == child.qualified_name())
                .map(index_definition)
                .collect::<StorageBackendResult<Vec<_>>>()?
                .into_iter()
                .filter(|definition| {
                    definition
                        .relationships
                        .parent_index
                        .is_some_and(|parent| removed_parent_indexes.contains(&parent))
                })
                .filter_map(|definition| definition.relationships.owning_constraint)
                .collect::<BTreeSet<_>>();
            remove_keys(&mut columns, &mut constraints, &removed_owners);
            let inherited = super::owners::append_keys(
                original,
                &child,
                &mut constraints.key_constraints,
                &parent_keys,
            )?;
            if constraints.key_constraints != before.key_constraints {
                for key in &constraints.key_constraints {
                    uqa_sql::schema::keys::apply_primary_key_columns(
                        &child.qualified_name(),
                        key,
                        &mut columns,
                    )
                    .map_err(invalid)?;
                }
                let view = CatalogReadView::new(candidate.clone());
                let names = partitions::CandidateNames {
                    catalog: &view,
                    rows: &candidate.definitions.catalog_indexes,
                };
                uqa_sql::schema::indexes::names::name_constraint_indexes(
                    &names,
                    &child.qualified_name(),
                    &mut constraints.key_constraints,
                )
                .map_err(|error| StorageBackendError::backend("partition key names", error))?;
                constraints
                    .hierarchy
                    .partition_inherited_key_constraints
                    .extend(
                        constraints.key_constraints
                            [constraints.key_constraints.len() - inherited.len()..]
                            .iter()
                            .cloned(),
                    );
                let event_names = crate::schema::constraints::names::event_names(original, &child);
                materialize_constraint_metadata_with_names(
                    &child,
                    &mut columns,
                    &mut constraints,
                    allocator,
                    &event_names,
                )
                .map_err(metadata_error)?;
                replace(candidate, &child, &columns, &constraints)?;
                changes.push(OwnerChange {
                    relation: child.clone(),
                    object_id,
                    columns,
                    constraints,
                });
            }
            pending.push_back(child);
        }
    }
    Ok(changes)
}

pub(super) fn remove_keys(
    columns: &mut [ColumnDef],
    constraints: &mut TableConstraintSet,
    owners: &BTreeSet<[u8; 16]>,
) {
    let retained = |key: &uqa_sql::ast::TableKeyConstraint| {
        key.catalog_identity
            .is_none_or(|identity| !owners.contains(&identity.object_id))
    };
    for key in constraints
        .key_constraints
        .iter()
        .filter(|key| !retained(key))
    {
        if key.columns.len() == 1 {
            if let Some(column) = columns
                .iter_mut()
                .find(|column| column.name == key.columns[0])
            {
                match key.kind {
                    uqa_sql::ast::TableKeyConstraintKind::PrimaryKey => column.primary_key = false,
                    uqa_sql::ast::TableKeyConstraintKind::Unique => column.unique = false,
                }
            }
        }
    }
    constraints.key_constraints.retain(retained);
    constraints
        .hierarchy
        .partition_inherited_key_constraints
        .retain(retained);
}

fn removed_parent_indexes(
    original: &CatalogReadView,
    parent: &RelationIdentity,
    parent_keys: &[uqa_sql::ast::TableKeyConstraint],
) -> StorageBackendResult<BTreeSet<[u8; 16]>> {
    let removed = original
        .snapshot()
        .definitions
        .catalog_indexes
        .values()
        .filter(|row| row.table_name == parent.qualified_name())
        .map(index_definition)
        .collect::<StorageBackendResult<Vec<_>>>()?
        .into_iter()
        .filter(|definition| {
            definition
                .relationships
                .owning_constraint
                .is_some_and(|owner| {
                    !parent_keys.iter().any(|key| {
                        key.catalog_identity
                            .is_some_and(|identity| identity.object_id == owner)
                    })
                })
        })
        .filter_map(|definition| {
            definition
                .catalog
                .map(|identity| identity.identity.object_id)
        })
        .collect::<BTreeSet<_>>();
    Ok(removed)
}
