//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The constraints a table's foreign keys derive on the partitions of the tables they reference follow those tables' partition trees: each publication of the table reconciles them, and a change to a referenced partition tree republishes the tables whose foreign keys reference it, as `PostgreSQL`'s `CloneFkReferenced` and `DetachPartitionFinalize` create and drop them.

use std::collections::BTreeSet;

use super::{replace_constraint_state, SchemaPublicationContext};
use crate::row_locks::RelationLockMode;
use uqa_sql::ast::{ColumnDef, ForeignKey, TableConstraintSet};
use uqa_sql::schema::constraint_changes::names::ConstraintNames;
use uqa_sql::schema::constraint_metadata::CatalogIdentityAllocator;
use uqa_sql::schema::inheritance::foreign_keys::declared_foreign_key_families;
use uqa_sql::schema::referenced_partitions::{
    reconcile_table_referenced_partition_constraints, ReferencedPartition,
    ReferencedPartitionSource,
};
use uqa_sql::SQLError;
use uqa_storage::{StorageBackendError, StorageBackendResult};

/// The relations of the catalog and the relation locks of the session, which republishing the referencing tables of a referenced partition tree needs.
pub trait ReferencingTableAccess {
    fn table_names(&self) -> StorageBackendResult<Vec<String>>;
    fn lock_relation(&self, table: &str, mode: RelationLockMode) -> Result<(), SQLError>;
}

struct PublicationSource<'a, 'b> {
    context: &'a SchemaPublicationContext<'b>,
}

fn storage(error: StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error("derived constraint publication", &error)
}

fn declared_state(
    context: &SchemaPublicationContext<'_>,
    table: &str,
) -> Result<(Vec<ColumnDef>, TableConstraintSet), SQLError> {
    let state = context
        .catalog
        .table_state(table)
        .map_err(storage)?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    Ok((state.columns(), state.constraints()))
}

impl ReferencedPartitionSource for PublicationSource<'_, '_> {
    fn referenced_partitions(&self, table: &str) -> Result<Vec<ReferencedPartition>, SQLError> {
        let root = self
            .context
            .partitions
            .catalog
            .try_table_object_id(table)
            .map_err(SQLError::Internal)?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        Ok(
            uqa_sql::semantics::partition::partition_tree(&self.context.partitions, table, false)?
                .into_iter()
                .map(|node| ReferencedPartition {
                    partition: node.object_id,
                    parent: (node.parent_object_id != root).then_some(node.parent_object_id),
                })
                .collect(),
        )
    }

    fn declared_foreign_key_ids(&self, table: &str) -> Result<BTreeSet<[u8; 16]>, SQLError> {
        let (columns, constraints) = declared_state(self.context, table)?;
        Ok(declared_foreign_key_families(&columns, &constraints))
    }
}

/// Reconcile the derived constraints of a publication candidate, naming new ones against `schema`, the constraint names of the candidate's schema, and the candidate's own names.
pub(super) fn reconcile_derived_constraints(
    context: &SchemaPublicationContext<'_>,
    columns: &mut [ColumnDef],
    constraints: &mut TableConstraintSet,
    schema: &BTreeSet<String>,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> StorageBackendResult<bool> {
    let mut used = schema.clone();
    used.extend(
        ConstraintNames::from_definition(columns, constraints)
            .entries()
            .map(|entry| entry.name.to_string()),
    );
    reconcile_table_referenced_partition_constraints(
        &PublicationSource { context },
        columns,
        constraints,
        &mut used,
        allocate,
    )
    .map_err(|error| StorageBackendError::backend("derived constraints", error))
}

/// The foreign keys without a parent that a declaration holds: the copies of a partitioned parent's foreign keys are left out.
fn declared_foreign_keys(
    context: &SchemaPublicationContext<'_>,
    columns: &[ColumnDef],
    constraints: &TableConstraintSet,
) -> Result<Vec<ForeignKey>, SQLError> {
    let inherited = match constraints
        .hierarchy
        .parents
        .first()
        .filter(|_| constraints.hierarchy.is_partition())
    {
        Some(parent) => {
            let (parent_columns, parent_constraints) = declared_state(context, parent)?;
            declared_foreign_key_families(&parent_columns, &parent_constraints)
        }
        None => BTreeSet::new(),
    };
    Ok(columns
        .iter()
        .filter_map(|column| {
            column.references.as_ref().map(|reference| {
                uqa_sql::schema::foreign_keys::column_foreign_key(column, reference)
            })
        })
        .chain(constraints.foreign_keys.iter().cloned())
        .filter(|foreign_key| {
            foreign_key
                .object_id
                .is_none_or(|object_id| !inherited.contains(&object_id))
        })
        .collect())
}

/// Republish every table with a foreign key without a parent that references `parent` or a partitioned table `parent` is a partition of, so the constraints it derives follow a partition that joined or left `parent`. Each such table is locked `mode`: `ShareRowExclusive` to derive constraints on a partition that joins, as `CloneFkReferenced` does, and `AccessExclusive` to drop those of a partition that leaves, as dropping their rows does.
pub fn republish_referencing_tables(
    context: &SchemaPublicationContext<'_>,
    parent: &str,
    mode: RelationLockMode,
) -> Result<(), SQLError> {
    let referenced = uqa_sql::semantics::partition::partition_ancestor_tables(
        context.partitions.catalog,
        parent,
    )?
    .into_iter()
    .collect::<BTreeSet<_>>();
    for table in context.referencing.table_names().map_err(storage)? {
        let (columns, constraints) = declared_state(context, &table)?;
        if !declared_foreign_keys(context, &columns, &constraints)?
            .iter()
            .any(|foreign_key| referenced.contains(&foreign_key.ref_table))
        {
            continue;
        }
        context.referencing.lock_relation(&table, mode)?;
        let (columns, constraints) = declared_state(context, &table)?;
        replace_constraint_state(context, &table, columns, constraints).map_err(storage)?;
    }
    Ok(())
}

/// Whether a foreign key without a parent holds derived constraints that do not match the partition tree of the table it references, as foreign keys of releases that did not derive them do.
pub fn derived_constraints_need_repair(
    context: &SchemaPublicationContext<'_>,
) -> Result<bool, SQLError> {
    let source = PublicationSource { context };
    for table in context.referencing.table_names().map_err(storage)? {
        let (columns, constraints) = declared_state(context, &table)?;
        for foreign_key in declared_foreign_keys(context, &columns, &constraints)? {
            let tree = source.referenced_partitions(&foreign_key.ref_table)?;
            let held = foreign_key
                .referenced_partitions
                .iter()
                .map(|derived| (derived.partition, derived.parent))
                .collect::<Vec<_>>();
            let expected = tree
                .iter()
                .map(|partition| (partition.partition, partition.parent))
                .collect::<Vec<_>>();
            if held != expected {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Republish the tables whose foreign keys hold derived constraints that do not match their referenced partition trees, which names and identifies the constraints they lack as if the foreign keys were created now.
pub fn repair_derived_constraints(context: &SchemaPublicationContext<'_>) -> Result<(), SQLError> {
    let source = PublicationSource { context };
    for table in context.referencing.table_names().map_err(storage)? {
        let (columns, constraints) = declared_state(context, &table)?;
        let mut stale = false;
        for foreign_key in declared_foreign_keys(context, &columns, &constraints)? {
            let tree = source.referenced_partitions(&foreign_key.ref_table)?;
            stale |= foreign_key
                .referenced_partitions
                .iter()
                .map(|derived| (derived.partition, derived.parent))
                .ne(tree
                    .iter()
                    .map(|partition| (partition.partition, partition.parent)));
        }
        if stale {
            replace_constraint_state(context, &table, columns, constraints).map_err(storage)?;
        }
    }
    Ok(())
}
