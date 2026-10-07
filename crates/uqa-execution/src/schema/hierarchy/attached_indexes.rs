//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The indexes that an attached partition builds for the unique indexes of its new parent, checked as `PostgreSQL`'s `AttachPartitionEnsureIndexes` and `DefineIndex` build them.

use super::{ddl_storage_error, HierarchyContext};
use crate::catalog::{index::index_definition, CatalogReadView};
use crate::schema::indexes::unique_build::{
    validate_unique_index_build, UniqueBuildContext, UniqueIndexBuild,
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;
use uqa_sql::ast::{IndexKey, TableKeyConstraint, TableKeyConstraintKind};
use uqa_sql::catalog::index::IndexDefinition;
use uqa_sql::schema::indexes::unique::{validate_partitioned_unique_key, PartitionedUniqueKey};
use uqa_sql::SQLError;
use uqa_storage::CatalogIndexRow;

/// The attached subtree as it was before the attachment, which tells an index the subtree already had from one the attachment built.
pub(super) struct PriorIndexes {
    /// The attached table and the partitions below it, each after its parent, with the table each belongs to.
    tables: Vec<(String, String)>,
    indexes: BTreeSet<RelationIdentity>,
    primary_keys: BTreeSet<String>,
}

fn storage(error: uqa_storage::StorageBackendError) -> SQLError {
    ddl_storage_error("ATTACH PARTITION indexes", error)
}

fn relation(table: &str) -> Result<RelationIdentity, SQLError> {
    RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)
}

pub(super) fn prior_indexes(
    context: &HierarchyContext<'_>,
    parent: &str,
    partition: &str,
) -> Result<PriorIndexes, SQLError> {
    let mut tables = vec![(partition.to_string(), parent.to_string())];
    tables.extend(
        uqa_sql::semantics::partition::partition_tree(&context.partitions, partition, false)?
            .into_iter()
            .map(|node| (node.table, node.parent)),
    );
    let catalog = context
        .publication
        .identities
        .catalog
        .current_catalog_snapshot();
    let snapshot = catalog.snapshot();
    let mut primary_keys = BTreeSet::new();
    for (table, _) in &tables {
        if snapshot.tables.get(&relation(table)?).is_some_and(|state| {
            state
                .keys
                .iter()
                .any(|key| key.kind == TableKeyConstraintKind::PrimaryKey)
        }) {
            primary_keys.insert(table.clone());
        }
    }
    let indexes = snapshot
        .definitions
        .catalog_indexes
        .values()
        .filter(|row| tables.iter().any(|(table, _)| *table == row.table_name))
        .map(|row| row.relation.clone())
        .collect();
    Ok(PriorIndexes {
        tables,
        indexes,
        primary_keys,
    })
}

/// A unique index of the parent, with the key constraint that owns it.
struct ParentIndex<'a> {
    row: &'a CatalogIndexRow,
    object_id: [u8; 16],
    key: Option<&'a TableKeyConstraint>,
}

/// The unique indexes of `parent` in the order their partition indexes are built: the indexes of its keys in the order the keys were declared, then its other unique indexes by name.
fn parent_unique_indexes<'a>(
    catalog: &'a CatalogReadView,
    parent: &str,
) -> Result<Vec<ParentIndex<'a>>, SQLError> {
    let snapshot = catalog.snapshot();
    let keys = snapshot
        .tables
        .get(&relation(parent)?)
        .map(|state| state.keys.as_slice())
        .unwrap_or_default();
    let mut indexes = Vec::new();
    for row in snapshot
        .definitions
        .catalog_indexes
        .values()
        .filter(|row| row.table_name == parent)
    {
        let definition = index_definition(row).map_err(storage)?;
        if !definition.unique {
            continue;
        }
        let Some(identity) = definition.catalog.as_ref() else {
            continue;
        };
        let position = definition
            .relationships
            .owning_constraint
            .and_then(|owner| {
                keys.iter().position(|key| {
                    key.catalog_identity
                        .is_some_and(|identity| identity.object_id == owner)
                })
            });
        indexes.push((
            position.unwrap_or(usize::MAX),
            ParentIndex {
                row,
                object_id: identity.identity.object_id,
                key: position.map(|position| &keys[position]),
            },
        ));
    }
    indexes.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.row.relation.cmp(&right.1.row.relation))
    });
    Ok(indexes.into_iter().map(|(_, index)| index).collect())
}

/// Check each index that the attachment built on the attached subtree, one unique index of the parent after another and each partition after its parent: a primary key's index beside a primary key the partition already had, a partitioned partition's index against its own partition key, and a leaf's rows as building its index does. A partition that adopted an index it already had built nothing, in itself or below.
pub(super) fn validate_attached_indexes(
    context: &HierarchyContext<'_>,
    parent: &str,
    prior: &PriorIndexes,
) -> Result<(), SQLError> {
    let catalog = context
        .publication
        .identities
        .catalog
        .current_catalog_snapshot();
    let snapshot = catalog.snapshot();
    let rows = &snapshot.definitions.catalog_indexes;
    for index in parent_unique_indexes(&catalog, parent)? {
        let kind = index
            .key
            .map_or(TableKeyConstraintKind::Unique, |key| key.kind);
        // The index of each table that built one, which its partitions' indexes attach to.
        let mut built = BTreeMap::from([(parent, index.object_id)]);
        for (table, table_parent) in &prior.tables {
            let Some(parent_index) = built.get(table_parent.as_str()).copied() else {
                continue;
            };
            let mut child = None;
            for row in rows.values().filter(|row| row.table_name == *table) {
                let definition = index_definition(row).map_err(storage)?;
                if definition.relationships.parent_index == Some(parent_index) {
                    child = Some((row, definition));
                    break;
                }
            }
            let Some((row, definition)) = child else {
                return Err(SQLError::Internal(format!(
                    "attached partition `{table}` has no index for `{}`",
                    index.row.relation.name
                )));
            };
            if prior.indexes.contains(&row.relation) {
                continue;
            }
            let Some(identity) = definition.catalog.as_ref() else {
                return Err(SQLError::Internal(format!(
                    "index `{}` has no identity",
                    row.relation.name
                )));
            };
            built.insert(table.as_str(), identity.identity.object_id);
            if kind == TableKeyConstraintKind::PrimaryKey && prior.primary_keys.contains(table) {
                return Err(uqa_sql::schema::keys::definition::multiple_primary_keys(
                    &relation(table)?.name,
                ));
            }
            let state = snapshot
                .tables
                .get(&relation(table)?)
                .ok_or_else(|| SQLError::UnknownTable(table.clone()))?;
            let keys: Vec<IndexKey> =
                serde_json::from_str(&row.columns_json).map_err(|error| storage(error.into()))?;
            if let Some(partition) = &state.hierarchy.partition_spec {
                let columns = keys.iter().map(IndexKey::column).collect::<Vec<_>>();
                validate_partitioned_unique_key(
                    table,
                    &PartitionedUniqueKey {
                        constraint_type: kind,
                        columns: &columns,
                        without_overlaps: index.key.is_some_and(|key| key.without_overlaps),
                    },
                    partition,
                )?;
                continue;
            }
            build_leaf_index(context, table, &state.keys, row, &definition, &keys)?;
        }
    }
    Ok(())
}

/// Check a leaf's rows as building its new index does. The index of a key constraint is built as the key's index, which covers a `WITHOUT OVERLAPS` period.
fn build_leaf_index(
    context: &HierarchyContext<'_>,
    table: &str,
    table_keys: &[TableKeyConstraint],
    row: &CatalogIndexRow,
    definition: &IndexDefinition,
    keys: &[IndexKey],
) -> Result<(), SQLError> {
    if let Some(owner) = definition.relationships.owning_constraint {
        let key = table_keys
            .iter()
            .find(|key| {
                key.catalog_identity
                    .is_some_and(|identity| identity.object_id == owner)
            })
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "index `{}` has no owning constraint",
                    row.relation.name
                ))
            })?;
        return crate::schema::keys::validate_key_index_rows(
            &crate::schema::keys::KeyValidationContext {
                catalog: context.catalog,
                constraints: context.constraints,
            },
            table,
            key,
        );
    }
    validate_unique_index_build(
        UniqueBuildContext::of(&context.constraints),
        &UniqueIndexBuild {
            table,
            name: &row.relation.name,
            keys,
            key_types: &definition.key_types,
            predicate: definition.predicate.as_deref(),
            nulls_not_distinct: definition.nulls_not_distinct,
        },
    )
}
