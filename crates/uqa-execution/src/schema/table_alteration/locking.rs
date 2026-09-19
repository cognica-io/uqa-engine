//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind referenced and inherited ALTER targets before admitting the definition writer.

use super::{binding::TableAlterBindingContext, TableAlterContext};
use crate::row_locks::{
    binding::{bind_relation, lock_relation_identity, RelationBinding},
    RelationLockMode,
};
use std::collections::BTreeSet;
use uqa_sql::{
    ast::{AlterTableAction, PartitionBound},
    schema::relation_alteration::RelationAlterTarget,
    SQLError,
};

pub(super) fn prepare_table_alter_action<S: Clone + 'static>(
    binding: &TableAlterBindingContext<'_>,
    tables: &TableAlterContext<'_, S>,
    primary: &str,
    recurse: bool,
    action: &mut AlterTableAction,
    mode: RelationLockMode,
) -> Result<(), SQLError> {
    bind_secondary_relations(binding, tables, primary, action)?;
    lock_inherited_action(binding, tables, primary, recurse, action, mode)
}

fn bind_secondary_relations<S: Clone + 'static>(
    binding: &TableAlterBindingContext<'_>,
    tables: &TableAlterContext<'_, S>,
    primary: &str,
    action: &mut AlterTableAction,
) -> Result<(), SQLError> {
    match action {
        AlterTableAction::AddForeignKeyConstraint { constraint } => {
            constraint.ref_table = bind_secondary_table(
                binding,
                &constraint.ref_table,
                RelationLockMode::ShareRowExclusive,
                false,
            )?;
        }
        AlterTableAction::AddColumn { column, .. } => {
            let exists = tables
                .addition
                .state
                .has_column(primary, &column.name)
                .map_err(|error| super::ddl_storage_error("ALTER TABLE column locks", error))?;
            if let Some(reference) = column.references.as_mut().filter(|_| !exists) {
                reference.table = bind_secondary_table(
                    binding,
                    &reference.table,
                    RelationLockMode::ShareRowExclusive,
                    false,
                )?;
            }
        }
        AlterTableAction::AddInheritance { parent } => {
            *parent = bind_secondary_table(
                binding,
                parent,
                RelationLockMode::ShareUpdateExclusive,
                true,
            )?;
        }
        AlterTableAction::DropInheritance { parent } => {
            *parent = bind_secondary_table(binding, parent, RelationLockMode::AccessShare, false)?;
        }
        AlterTableAction::AttachPartition { partition, .. } => {
            *partition =
                bind_secondary_table(binding, partition, RelationLockMode::AccessExclusive, true)?;
            lock_partition_subtree(binding, tables, partition)?;
            for child in tables
                .hierarchy
                .partitions
                .catalog
                .direct_hierarchy_children(primary)?
            {
                let hierarchy = tables
                    .hierarchy
                    .partitions
                    .catalog
                    .try_table_hierarchy(&child)
                    .map_err(SQLError::Internal)?;
                if matches!(hierarchy.partition_bound, Some(PartitionBound::Default)) {
                    lock_partition_subtree(binding, tables, &child)?;
                }
            }
        }
        AlterTableAction::DetachPartition { partition, .. } => {
            *partition =
                bind_secondary_table(binding, partition, RelationLockMode::AccessExclusive, false)?;
            lock_partition_subtree(binding, tables, partition)?;
        }
        _ => {}
    }
    Ok(())
}

fn bind_secondary_table(
    context: &TableAlterBindingContext<'_>,
    requested: &str,
    mode: RelationLockMode,
    require_owner: bool,
) -> Result<String, SQLError> {
    let binding = bind_relation(
        context.locks,
        mode,
        false,
        || {
            let target = RelationAlterTarget::resolve(
                context.names.resolve_relation_kind(requested)?,
                requested,
                false,
                &mut |_| {},
            )?
            .ok_or_else(|| SQLError::Internal("required ALTER relation has no target".into()))?;
            Ok(Some(RelationBinding {
                object_id: context.catalog.relation_object_id(&target.canonical)?,
                name: target.canonical.clone(),
                value: target,
            }))
        },
        |binding| {
            let target = &binding.value;
            if require_owner {
                crate::schema::relation_alteration::validate_relation_alter_authority(
                    &context.authority,
                    &context.creation,
                    &target.relation,
                    target.kind,
                    false,
                )?;
            }
            target.require_kind("table")
        },
    )?
    .ok_or_else(|| SQLError::Internal("required ALTER relation disappeared".into()))?;
    Ok(binding.name)
}

fn lock_partition_subtree<S: Clone + 'static>(
    context: &TableAlterBindingContext<'_>,
    tables: &TableAlterContext<'_, S>,
    root: &str,
) -> Result<(), SQLError> {
    let Some(identity) = context.catalog.relation_object_id(root)? else {
        return Ok(());
    };
    let Some(current) = lock_relation_identity(
        context.catalog,
        context.locks,
        root.to_string(),
        identity,
        RelationLockMode::AccessExclusive,
        false,
    )?
    else {
        return Ok(());
    };
    lock_alter_children(
        context,
        &current,
        RelationLockMode::AccessExclusive,
        |parent| {
            tables
                .hierarchy
                .partitions
                .catalog
                .direct_hierarchy_children(parent)
        },
    )
}

fn lock_inherited_action<S: Clone + 'static>(
    context: &TableAlterBindingContext<'_>,
    tables: &TableAlterContext<'_, S>,
    root: &str,
    recurse: bool,
    action: &AlterTableAction,
    mode: RelationLockMode,
) -> Result<(), SQLError> {
    let all_descendants = locks_all_descendants(tables, root, recurse, action)?;
    if !all_descendants && !recurse {
        return Ok(());
    }
    lock_alter_children(context, root, mode, |parent| {
        if all_descendants {
            tables
                .hierarchy
                .partitions
                .catalog
                .direct_hierarchy_children(parent)
        } else if matches!(
            action,
            AlterTableAction::AddColumn { .. } | AlterTableAction::SetNotNull { .. }
        ) {
            super::recursion::recursive_alter_children(tables, parent, recurse, action)
        } else {
            Ok(Vec::new())
        }
    })
}

fn lock_alter_children(
    context: &TableAlterBindingContext<'_>,
    root: &str,
    mode: RelationLockMode,
    mut children: impl FnMut(&str) -> Result<Vec<String>, SQLError>,
) -> Result<(), SQLError> {
    let mut pending = vec![root.to_string()];
    let mut retained = BTreeSet::new();
    while let Some(parent) = pending.pop() {
        // Enumerate below each locked parent after refreshing: a wait may have admitted a new child before this lock was acquired.
        let targets = children(&parent)?
            .into_iter()
            .map(|name| {
                context
                    .catalog
                    .relation_object_id(&name)
                    .map(|id| id.map(|id| (name, id)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for (name, identity) in targets.into_iter().flatten() {
            if !retained.insert(identity) {
                continue;
            }
            if let Some(current) =
                lock_relation_identity(context.catalog, context.locks, name, identity, mode, false)?
            {
                pending.push(current);
            }
        }
    }
    Ok(())
}

fn locks_all_descendants<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    parent: &str,
    recurse: bool,
    action: &AlterTableAction,
) -> Result<bool, SQLError> {
    // PostgreSQL prepares ADD CONSTRAINT by locking every inheritor before execution can merge a constraint or stop its propagation with NO INHERIT.
    if matches!(
        action,
        AlterTableAction::AddCheckConstraint { .. }
            | AlterTableAction::AddNotNullConstraint { .. }
            | AlterTableAction::AddForeignKeyConstraint { .. }
    ) {
        return Ok(recurse);
    }
    if let AlterTableAction::ValidateConstraint { name }
    | AlterTableAction::RenameConstraint { from: name, .. } = action
    {
        let checks = context
            .constraints
            .catalog
            .try_check_constraint_definitions(parent)
            .map_err(|error| super::ddl_storage_error("ALTER TABLE constraint locks", error))?;
        return Ok(checks
            .iter()
            .find(|check| check.name.as_deref() == Some(name))
            .is_some_and(|check| {
                !check.no_inherit
                    && if matches!(action, AlterTableAction::ValidateConstraint { .. }) {
                        !check.validated
                    } else {
                        recurse
                    }
            }));
    }
    Ok(false)
}
