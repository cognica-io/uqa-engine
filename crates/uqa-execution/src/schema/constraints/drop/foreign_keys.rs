//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain foreign-key groups and their relation identities before dependency waits.

use super::{
    ddl_storage_error, publish_constraint_state, table_constraint_state, ConstraintAlterContext,
};
use crate::row_locks::{binding::lock_relation_identity, RelationLockMode};
use std::collections::BTreeSet;
use uqa_sql::{
    schema::constraint_changes::{
        foreign_key_target::ForeignKeyTarget, inheritance::ensure_inherited_constraint_removable,
    },
    SQLError,
};

pub struct ForeignKeyRemovalTarget {
    table: String,
    table_id: [u8; 16],
    constraint_id: [u8; 16],
}

/// Capture every selected constraint and partition clone before acquiring any dependent lock.
pub fn capture_foreign_key_dependencies(
    context: &ConstraintAlterContext<'_>,
    dependents: impl IntoIterator<Item = (String, String)>,
) -> Result<Vec<ForeignKeyRemovalTarget>, SQLError> {
    let mut identities = BTreeSet::new();
    let mut selected = BTreeSet::new();
    let mut targets = Vec::new();
    for (table, name) in dependents {
        let (columns, constraints) = table_constraint_state(context, &table)?;
        let Some(target) = ForeignKeyTarget::by_name(&columns, &constraints, &name)? else {
            continue;
        };
        identities.insert(target.object_id);
        if selected.insert((table.clone(), target.object_id)) {
            targets.push(capture_target(context, table, target.object_id)?);
        }
    }
    if identities.is_empty() {
        return Ok(targets);
    }
    for table in context
        .relations
        .table_names()
        .map_err(|error| ddl_storage_error("DROP CONSTRAINT partition lookup", error))?
    {
        let (columns, constraints) = table_constraint_state(context, &table)?;
        for &identity in &identities {
            if !selected.contains(&(table.clone(), identity))
                && ForeignKeyTarget::by_id(&columns, &constraints, identity)?.is_some()
            {
                selected.insert((table.clone(), identity));
                targets.push(capture_target(context, table.clone(), identity)?);
            }
        }
    }
    Ok(targets)
}

fn capture_target(
    context: &ConstraintAlterContext<'_>,
    table: String,
    constraint_id: [u8; 16],
) -> Result<ForeignKeyRemovalTarget, SQLError> {
    let table_id = context
        .lock_catalog
        .relation_object_id(&table)?
        .ok_or_else(|| SQLError::UnknownTable(table.clone()))?;
    Ok(ForeignKeyRemovalTarget {
        table,
        table_id,
        constraint_id,
    })
}

pub(super) fn ensure_direct_removal(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    name: &str,
    object_id: [u8; 16],
) -> Result<(), SQLError> {
    let (_, constraints) = table_constraint_state(context, table)?;
    let mut inherited = constraints
        .hierarchy
        .partition_inherited_foreign_keys
        .iter()
        .any(|foreign_key| foreign_key.object_id == Some(object_id));
    for parent in &constraints.hierarchy.parents {
        let (columns, constraints) = table_constraint_state(context, parent)?;
        if ForeignKeyTarget::by_id(&columns, &constraints, object_id)?.is_some() {
            inherited = true;
            break;
        }
    }
    ensure_inherited_constraint_removable(table, name, usize::from(inherited))
}

pub fn drop_foreign_key_dependencies(
    context: &ConstraintAlterContext<'_>,
    targets: Vec<ForeignKeyRemovalTarget>,
) -> Result<(), SQLError> {
    drop_targets(context, targets, false)
}

pub(super) fn drop_targets(
    context: &ConstraintAlterContext<'_>,
    targets: Vec<ForeignKeyRemovalTarget>,
    direct: bool,
) -> Result<(), SQLError> {
    for target in targets {
        let Some(table) = lock_relation_identity(
            context.lock_catalog,
            context.lock_session,
            target.table,
            target.table_id,
            RelationLockMode::AccessExclusive,
            false,
        )?
        else {
            continue;
        };
        let (columns, constraints) = table_constraint_state(context, &table)?;
        if ForeignKeyTarget::by_id(&columns, &constraints, target.constraint_id)?.is_none() {
            continue;
        }
        // A dependency cascade removes child-side pending events; a direct drop rejects them.
        if direct {
            context
                .access
                .ensure_no_pending_events(&table, "ALTER TABLE")?;
        }
        drop_one(context, &table, target.constraint_id)?;
    }
    Ok(())
}

pub(super) fn drop_one(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    constraint_id: [u8; 16],
) -> Result<(), SQLError> {
    let (columns, constraints) = table_constraint_state(context, table)?;
    let Some(target) = ForeignKeyTarget::by_id(&columns, &constraints, constraint_id)? else {
        return Ok(());
    };
    let reference = target.referenced_table.to_string();
    let reference_id = context
        .lock_catalog
        .relation_object_id(&reference)?
        .ok_or_else(|| SQLError::UnknownTable(reference.clone()))?;
    let reference = lock_relation_identity(
        context.lock_catalog,
        context.lock_session,
        reference,
        reference_id,
        RelationLockMode::AccessExclusive,
        false,
    )?;
    // Reference renames rewrite the referring metadata while this acquisition waits.
    let (mut columns, mut constraints) = table_constraint_state(context, table)?;
    if ForeignKeyTarget::by_id(&columns, &constraints, constraint_id)?.is_none() {
        return Ok(());
    }
    let reference = reference.ok_or_else(|| {
        SQLError::Internal("FOREIGN KEY survived removal of its referenced table".into())
    })?;
    context
        .access
        .ensure_no_pending_events(&reference, "ALTER TABLE")?;
    uqa_sql::schema::constraint_changes::foreign_key_target::remove_foreign_key(
        &mut columns,
        &mut constraints,
        constraint_id,
    )?;
    publish_constraint_state(context, table, columns, constraints)
}
