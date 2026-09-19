//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve local constraint rows while separating detached foreign-key enforcement families.

use crate::ast::{ColumnDef, TableConstraintSet};
use crate::catalog::constraints::{
    constraint_identities_match, foreign_key_identity, ConstraintIdentity,
};
use crate::SQLError;
use std::collections::BTreeMap;

pub type ConstraintIdentityChange = (ConstraintIdentity, ConstraintIdentity);

pub fn split_foreign_key_families(
    table: &str,
    columns: &mut [ColumnDef],
    constraints: &mut TableConstraintSet,
    replacements: &BTreeMap<[u8; 16], [u8; 16]>,
) -> Result<Vec<ConstraintIdentityChange>, SQLError> {
    let mut changes = Vec::new();
    for column in columns {
        let Some(reference) = column.references.as_ref() else {
            continue;
        };
        let Some(replacement) = reference.object_id.and_then(|id| replacements.get(&id)) else {
            continue;
        };
        let before = foreign_key_identity(
            table,
            &crate::schema::foreign_keys::column_foreign_key(column, reference),
        )?;
        column
            .references
            .as_mut()
            .expect("selected reference")
            .object_id = Some(*replacement);
        let mut after = before.clone();
        after.object_id = Some(*replacement);
        changes.push((before, after));
    }
    for key in &mut constraints.foreign_keys {
        let Some(replacement) = key.object_id.and_then(|id| replacements.get(&id)) else {
            continue;
        };
        let before = foreign_key_identity(table, key)?;
        key.object_id = Some(*replacement);
        changes.push((before, foreign_key_identity(table, key)?));
    }
    for key in &mut constraints.hierarchy.partition_inherited_foreign_keys {
        if let Some(replacement) = key.object_id.and_then(|id| replacements.get(&id)) {
            key.object_id = Some(*replacement);
        }
    }
    Ok(changes)
}

/// Materialize each split family's prior explicit mode on both resulting groups. An ALL mode remains session-owned and needs no per-row replacement.
pub fn preserve_split_constraint_modes(
    named: &mut BTreeMap<ConstraintIdentity, bool>,
    retained: &[ConstraintIdentity],
    detached: &[ConstraintIdentityChange],
) {
    let changes: Vec<_> = retained
        .iter()
        .map(|identity| (identity, identity))
        .chain(detached.iter().map(|(before, after)| (before, after)))
        .filter_map(|(before, after)| {
            named.iter().find_map(|(identity, deferred)| {
                constraint_identities_match(before, identity).then_some((after.clone(), *deferred))
            })
        })
        .collect();
    named.extend(changes);
}

#[cfg(test)]
mod tests;
