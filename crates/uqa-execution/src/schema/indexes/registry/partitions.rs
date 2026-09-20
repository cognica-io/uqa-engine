//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialize index ancestry from prepared table and constraint candidates.

use super::{index_definition, invalid, metadata_error, IndexCatalogIdentity, IndexDefinition};
use crate::catalog::CatalogReadView;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{IndexKey, TableKeyConstraint, TableKeyConstraintKind},
    catalog::index::{can_attach_index, IndexAttachmentShape},
    schema::{constraint_metadata::CatalogObjectAllocator, indexes::names::IndexNameCatalog},
    SQLError,
};
use uqa_storage::{CatalogIndexRow, StorageBackendResult};

pub(in crate::schema::indexes) fn materialize(
    catalog: &CatalogReadView,
    rows: &mut BTreeMap<RelationIdentity, CatalogIndexRow>,
    allocator: &mut dyn CatalogObjectAllocator,
) -> StorageBackendResult<()> {
    let mut pending = rows.keys().cloned().collect::<VecDeque<_>>();
    let mut visited = BTreeSet::new();
    while let Some(name) = pending.pop_front() {
        let parent = rows
            .get(&name)
            .ok_or_else(|| invalid("index candidate disappeared"))?
            .clone();
        let parent_definition = index_definition(&parent)?;
        let identity = parent_definition
            .catalog
            .as_ref()
            .ok_or_else(|| invalid("partitioned index has no identity"))?;
        if !visited.insert(identity.identity.object_id) {
            continue;
        }
        let table = RelationIdentity::from_legacy_name(&parent.table_name).map_err(invalid)?;
        let state = catalog
            .snapshot()
            .tables
            .get(&table)
            .ok_or_else(|| invalid("partitioned index has no table"))?;
        if state.hierarchy.partition_spec.is_none() {
            continue;
        }
        let parent_keys: Vec<IndexKey> = serde_json::from_str(&parent.columns_json)?;
        let parent_kind = constraint_kind(catalog, &parent, &parent_definition)?;
        for (child_name, child) in catalog.snapshot().tables.iter().filter(|(_, child)| {
            child.hierarchy.is_partition()
                && child
                    .hierarchy
                    .parents
                    .first()
                    .is_some_and(|name| name == &parent.table_name)
        }) {
            let chosen = matching_child(
                catalog,
                rows,
                child_name,
                identity.identity.object_id,
                &IndexAttachmentShape {
                    method: &parent.index_type,
                    keys: &parent_keys,
                    definition: &parent_definition,
                    constraint_kind: parent_kind,
                },
            )?;
            let mut row = if let Some(chosen) = chosen {
                rows.get(&chosen).expect("retained candidate").clone()
            } else {
                if parent_kind.is_some() {
                    return Err(invalid(format!(
                        "prepared partition `{}` has no matching owned key index",
                        child_name.qualified_name()
                    )));
                }
                let name = uqa_sql::schema::indexes::names::allocate_default_index_name(
                    &CandidateNames { catalog, rows },
                    child_name,
                    &parent_keys,
                )
                .map_err(|error| uqa_storage::StorageBackendError::backend("index name", error))?;
                let mut definition = parent_definition.clone();
                definition.catalog = Some(
                    IndexCatalogIdentity::allocate(child.object_id, allocator)
                        .map_err(metadata_error)?,
                );
                definition.relationships.owning_constraint = None;
                definition.relationships.parent_index = None;
                CatalogIndexRow {
                    relation: RelationIdentity::new(&child_name.schema, name),
                    table_name: child_name.qualified_name(),
                    definition_json: Some(serde_json::to_string(&definition)?),
                    ..parent.clone()
                }
            };
            let mut definition = index_definition(&row)?;
            definition.relationships.parent_index = Some(identity.identity.object_id);
            row.definition_json = Some(serde_json::to_string(&definition)?);
            pending.push_back(row.relation.clone());
            rows.insert(row.relation.clone(), row);
        }
    }
    Ok(())
}

pub(in crate::schema::indexes) fn constraint_kind(
    catalog: &CatalogReadView,
    row: &CatalogIndexRow,
    definition: &IndexDefinition,
) -> StorageBackendResult<Option<TableKeyConstraintKind>> {
    let Some(owner) = definition.relationships.owning_constraint else {
        return Ok(None);
    };
    let relation = RelationIdentity::from_legacy_name(&row.table_name).map_err(invalid)?;
    let kind = catalog
        .snapshot()
        .tables
        .get(&relation)
        .and_then(|table| {
            table
                .keys
                .iter()
                .find(|key| key.catalog_identity.is_some_and(|id| id.object_id == owner))
        })
        .map(|key| key.kind)
        .ok_or_else(|| invalid("index has no owning constraint"))?;
    Ok(Some(kind))
}

pub(super) struct CandidateNames<'a> {
    pub catalog: &'a CatalogReadView,
    pub rows: &'a BTreeMap<RelationIdentity, CatalogIndexRow>,
}

impl IndexNameCatalog for CandidateNames<'_> {
    fn existing_constraint_names(
        &self,
        table: &str,
    ) -> Result<std::collections::BTreeSet<String>, SQLError> {
        let relation = RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
        Ok(crate::schema::constraints::names::existing_names(
            self.catalog,
            &relation,
        ))
    }
    fn existing_constraint_keys(&self, table: &str) -> Result<Vec<TableKeyConstraint>, SQLError> {
        let table = RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
        Ok(self
            .catalog
            .snapshot()
            .tables
            .get(&table)
            .map(|table| table.keys.as_ref().clone())
            .unwrap_or_default())
    }

    fn relation_name_available(&self, name: &str) -> Result<bool, SQLError> {
        let name = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        let snapshot = self.catalog.snapshot();
        Ok(!self.rows.contains_key(&name)
            && !snapshot.tables.contains_key(&name)
            && !snapshot.definitions.views.contains_key(&name)
            && !snapshot.definitions.foreign_tables.contains_key(&name)
            && !snapshot.definitions.sequences.contains_key(&name)
            && !snapshot.tables.iter().any(|(table, state)| {
                table.schema == name.schema
                    && state
                        .keys
                        .iter()
                        .any(|key| key.name.as_ref() == Some(&name.name))
            })
            && !uqa_sql::catalog::SystemRelation::all().any(|relation| {
                relation.namespace() == name.schema && relation.name() == name.name
            })
            && !crate::catalog::graph::graph_catalog_entries(self.catalog)?
                .iter()
                .any(|graph| {
                    graph.name == name.schema
                        && (name.name == "_label_id_seq"
                            || graph.labels.iter().any(|label| {
                                name.name == label.name
                                    || name.name == format!("{}_id_seq", label.name)
                            }))
                }))
    }
}

/// Detachment retains the child's independent address and contents while removing only the severed inheritance edge.
pub(super) fn detach(
    catalog: &CatalogReadView,
    rows: &mut BTreeMap<RelationIdentity, CatalogIndexRow>,
) -> StorageBackendResult<()> {
    let parents = rows
        .values()
        .map(|row| {
            Ok((
                index_definition(row)?
                    .catalog
                    .ok_or_else(|| invalid("index has no identity"))?
                    .identity
                    .object_id,
                row.table_name.clone(),
            ))
        })
        .collect::<StorageBackendResult<BTreeMap<_, _>>>()?;
    for row in rows.values_mut() {
        let mut definition = index_definition(row)?;
        let Some(parent) = definition.relationships.parent_index else {
            continue;
        };
        let parent = parents
            .get(&parent)
            .ok_or_else(|| invalid("index parent disappeared"))?;
        let relation = RelationIdentity::from_legacy_name(&row.table_name).map_err(invalid)?;
        let hierarchy = &catalog.snapshot().tables[&relation].hierarchy;
        if !hierarchy.is_partition() || hierarchy.parents.first() != Some(parent) {
            definition.relationships.parent_index = None;
            row.definition_json = Some(serde_json::to_string(&definition)?);
        }
    }
    Ok(())
}

fn matching_child(
    catalog: &CatalogReadView,
    rows: &BTreeMap<RelationIdentity, CatalogIndexRow>,
    child: &RelationIdentity,
    parent_id: [u8; 16],
    parent: &IndexAttachmentShape<'_>,
) -> StorageBackendResult<Option<RelationIdentity>> {
    let mut choices = Vec::new();
    for row in rows
        .values()
        .filter(|row| row.table_name == child.qualified_name())
    {
        let definition = index_definition(row)?;
        let keys: Vec<IndexKey> = serde_json::from_str(&row.columns_json)?;
        let already_attached = definition.relationships.parent_index == Some(parent_id);
        if definition.relationships.parent_index.is_some() && !already_attached {
            continue;
        }
        if can_attach_index(
            parent,
            &IndexAttachmentShape {
                method: &row.index_type,
                keys: &keys,
                constraint_kind: constraint_kind(catalog, row, &definition)?,
                definition: &definition,
            },
        ) {
            let address = definition
                .catalog
                .as_ref()
                .ok_or_else(|| invalid("child index has no identity"))?;
            choices.push((
                !already_attached,
                address.identity.oid,
                row.relation.clone(),
            ));
        } else if already_attached {
            return Err(invalid(
                "attached index definition disagrees with its parent",
            ));
        }
    }
    choices.sort();
    Ok(choices.into_iter().next().map(|(_, _, name)| name))
}
