//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain local schema objects when legacy hierarchy edges lose their parent.

use super::{alter, origins::InheritanceOriginChange};
use crate::{
    ast::{AutoIncrement, ColumnDef, TableConstraintSet},
    SQLError,
};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;

pub struct ParentEdgeRepair {
    pub changed: bool,
    pub origins: Option<InheritanceOriginChange>,
    pub detached_partition: bool,
    pub inherited_identity: Vec<(String, AutoIncrement)>,
}

/// Normalize legacy names and remove absent parents without removing constraints belonging to the surviving relation. The caller applies origin changes against the remaining parents and separates detached subtree enforcement families.
pub fn repair_parent_edges(
    columns: &mut [ColumnDef],
    constraints: &mut TableConstraintSet,
    existing: &BTreeSet<String>,
) -> Result<ParentEdgeRepair, SQLError> {
    let previous = constraints.hierarchy.clone();
    let mut parents = Vec::with_capacity(previous.parents.len());
    let mut sequence_numbers = Vec::with_capacity(previous.parents.len());
    for (index, parent) in previous.parents.iter().enumerate() {
        let parent = RelationIdentity::from_legacy_name(parent)
            .map_err(SQLError::Internal)?
            .qualified_name();
        if existing.contains(&parent) {
            parents.push(parent);
            sequence_numbers.push(previous.parent_sequence_number(index));
        }
    }
    let changed = parents != previous.parents;
    let detached_partition = changed && parents.is_empty() && previous.is_partition();
    let mut inherited_identity = Vec::new();
    if changed {
        constraints.hierarchy.parents = parents;
        constraints.hierarchy.parent_sequence_numbers = sequence_numbers;
        if constraints.hierarchy.parents.is_empty() {
            constraints.hierarchy.local_columns =
                columns.iter().map(|column| column.name.clone()).collect();
        }
        if detached_partition {
            inherited_identity = columns
                .iter()
                .filter_map(|column| {
                    column
                        .auto_increment
                        .as_ref()
                        .filter(|increment| increment.is_identity())
                        .map(|increment| (column.name.clone(), increment.clone()))
                })
                .collect();
            restore_partition_identity(columns, constraints, &inherited_identity);
            constraints.hierarchy.partition_bound = None;
            alter::clear_partition_constraint_provenance(constraints);
        }
    }
    Ok(ParentEdgeRepair {
        changed,
        origins: InheritanceOriginChange::between(&previous, &constraints.hierarchy),
        detached_partition,
        inherited_identity,
    })
}

pub fn restore_partition_identity(
    columns: &mut [ColumnDef],
    constraints: &mut TableConstraintSet,
    inherited: &[(String, AutoIncrement)],
) {
    alter::restore_identity_overrides(
        columns,
        inherited,
        &constraints.hierarchy.partition_identity_overrides,
    );
    constraints.hierarchy.partition_identity_overrides.clear();
}

#[cfg(test)]
mod tests;
