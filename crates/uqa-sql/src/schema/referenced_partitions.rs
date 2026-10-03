//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The constraints a foreign key derives on the partitions of the partitioned table it references, named and identified as `PostgreSQL`'s `addFkRecurseReferenced` and `CloneFkReferenced` create them and removed as `DetachPartitionFinalize` and `AttachPartitionForeignKey` remove them.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{ColumnDef, ReferencedPartitionConstraint, TableConstraintSet};
use crate::schema::constraint_metadata::{
    CatalogIdentityAllocator, ConstraintMetadataError, ConstraintMetadataResult,
};
use crate::SQLError;

#[cfg(test)]
mod tests;

/// A referenced partition and the partitioned partition it belongs to, or `None` when it belongs to the foreign key's referenced table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferencedPartition {
    pub partition: [u8; 16],
    pub parent: Option<[u8; 16]>,
}

/// The state of the foreign key that its derived constraints follow.
#[derive(Debug, Clone, Copy)]
pub struct ReferencingConstraint<'a> {
    pub name: &'a str,
    pub validated: bool,
    pub enforced: bool,
}

/// What derivation reads from the catalog.
pub trait ReferencedPartitionSource {
    /// The partitions below `table`, each before its own partitions, with siblings in partition order; empty when `table` is not partitioned.
    fn referenced_partitions(&self, table: &str) -> Result<Vec<ReferencedPartition>, SQLError>;
    /// The object identities of the foreign keys `table` declares, which its partitions' copies share.
    fn declared_foreign_key_ids(&self, table: &str) -> Result<BTreeSet<[u8; 16]>, SQLError>;
}

/// Make `constraints` hold one derived constraint for each of `partitions`, the referenced table's partition tree in order, keeping the constraints of partitions that remain under the same parent. The partitions that joined below one constraint since the constraints were last derived take their names from that constraint and its validity, as `PostgreSQL` recurses into a new partition subtree with the name of the constraint it joins: the foreign key's own for a partition of the referenced table, else the derived constraint of the partition's parent. A valid foreign key validates every derived constraint, and one that is not enforced validates none. Reports whether the constraints changed.
pub fn reconcile_referenced_partition_constraints(
    constraints: &mut Vec<ReferencedPartitionConstraint>,
    foreign_key: ReferencingConstraint<'_>,
    partitions: &[ReferencedPartition],
    used: &mut BTreeSet<String>,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> ConstraintMetadataResult<bool> {
    let previous = std::mem::take(constraints);
    let mut kept = previous
        .iter()
        .filter(|constraint| {
            partitions.iter().any(|partition| {
                partition.partition == constraint.partition && partition.parent == constraint.parent
            })
        })
        .map(|constraint| (constraint.partition, constraint.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut joined = BTreeMap::<[u8; 16], (String, bool)>::new();
    let mut reconciled = Vec::with_capacity(partitions.len());
    for partition in partitions {
        if let Some(constraint) = kept.remove(&partition.partition) {
            reconciled.push(constraint);
            continue;
        }
        let (base, validated) = match partition.parent {
            None => (foreign_key.name.to_string(), foreign_key.validated),
            Some(parent) => joined
                .get(&parent)
                .cloned()
                .or_else(|| {
                    reconciled
                        .iter()
                        .find(|constraint: &&ReferencedPartitionConstraint| {
                            constraint.partition == parent
                        })
                        .map(|constraint| (constraint.name.clone(), constraint.validated))
                })
                .ok_or_else(|| {
                    ConstraintMetadataError::Invalid(
                        "a referenced partition precedes its parent in partition order".into(),
                    )
                })?,
        };
        let name = super::constraint_metadata::choose_suffixed_constraint_name(&base, used)?;
        let mut catalog_identity = None;
        super::constraint_metadata::identity::foreign_keys::materialize(
            &mut catalog_identity,
            allocate,
        )?;
        let catalog_identity = catalog_identity.ok_or_else(|| {
            ConstraintMetadataError::Invalid("a derived constraint has no catalog identity".into())
        })?;
        joined.insert(partition.partition, (base, validated));
        reconciled.push(ReferencedPartitionConstraint {
            partition: partition.partition,
            parent: partition.parent,
            name,
            catalog_identity,
            validated,
        });
    }
    for constraint in &mut reconciled {
        if foreign_key.validated {
            constraint.validated = true;
        }
        if !foreign_key.enforced {
            constraint.validated = false;
        }
    }
    let changed = reconciled != previous;
    *constraints = reconciled;
    Ok(changed)
}

fn source_error(error: SQLError) -> ConstraintMetadataError {
    ConstraintMetadataError::Execution(Box::new(error))
}

fn unnamed() -> ConstraintMetadataError {
    ConstraintMetadataError::Invalid("a foreign key derives constraints before it is named".into())
}

/// Reconcile the derived constraints of every foreign key a table declares with the current partition trees of the tables they reference. A copy of a foreign key that the table's partitioned parent declares derives none, as only the constraint without a parent derives constraints in `PostgreSQL`; a copy that became the table's own when the table was detached derives them under its own name. Reports whether any foreign key changed.
pub fn reconcile_table_referenced_partition_constraints(
    source: &dyn ReferencedPartitionSource,
    columns: &mut [ColumnDef],
    constraints: &mut TableConstraintSet,
    used: &mut BTreeSet<String>,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> ConstraintMetadataResult<bool> {
    let inherited = match constraints
        .hierarchy
        .parents
        .first()
        .filter(|_| constraints.hierarchy.is_partition())
    {
        Some(parent) => source
            .declared_foreign_key_ids(parent)
            .map_err(source_error)?,
        None => BTreeSet::new(),
    };
    let mut changed = false;
    for reference in columns
        .iter_mut()
        .filter_map(|column| column.references.as_mut())
    {
        if reference
            .object_id
            .is_some_and(|object_id| inherited.contains(&object_id))
        {
            changed |= !reference.referenced_partitions.is_empty();
            reference.referenced_partitions.clear();
            continue;
        }
        let partitions = source
            .referenced_partitions(&reference.table)
            .map_err(source_error)?;
        changed |= reconcile_referenced_partition_constraints(
            &mut reference.referenced_partitions,
            ReferencingConstraint {
                name: reference.name.as_deref().ok_or_else(unnamed)?,
                validated: reference.validated,
                enforced: reference.enforced,
            },
            &partitions,
            used,
            allocate,
        )?;
    }
    for foreign_key in &mut constraints.foreign_keys {
        if foreign_key
            .object_id
            .is_some_and(|object_id| inherited.contains(&object_id))
        {
            changed |= !foreign_key.referenced_partitions.is_empty();
            foreign_key.referenced_partitions.clear();
            continue;
        }
        let partitions = source
            .referenced_partitions(&foreign_key.ref_table)
            .map_err(source_error)?;
        changed |= reconcile_referenced_partition_constraints(
            &mut foreign_key.referenced_partitions,
            ReferencingConstraint {
                name: foreign_key.name.as_deref().ok_or_else(unnamed)?,
                validated: foreign_key.validated,
                enforced: foreign_key.enforced,
            },
            &partitions,
            used,
            allocate,
        )?;
    }
    Ok(changed)
}
