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
    schema::constraint_changes::validation::{constraint_validation, ConstraintValidationKind},
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
    lock_inherited_action(binding, tables, primary, recurse, action, mode)?;
    bind_key_index(tables, primary, action)
}

fn bind_key_index<S: Clone + 'static>(
    tables: &TableAlterContext<'_, S>,
    table: &str,
    action: &mut AlterTableAction,
) -> Result<(), SQLError> {
    let (name, mode) = match action {
        AlterTableAction::RenameConstraint { from, .. } => {
            (from, RelationLockMode::ShareUpdateExclusive)
        }
        AlterTableAction::DropConstraint { name, .. } => (name, RelationLockMode::AccessExclusive),
        _ => return Ok(()),
    };
    let (_, constraints) =
        crate::schema::constraints::table_constraint_state(&tables.constraints, table)?;
    let Some(owner) = constraints
        .key_constraints
        .iter()
        .find(|key| key.name.as_ref() == Some(name))
        .and_then(|key| key.catalog_identity)
    else {
        return Ok(());
    };
    let registry = &tables.constraints.publication.indexes;
    let catalog = registry.identities.catalog.current_catalog_snapshot();
    let mut identity = None;
    for row in catalog
        .catalog_indexes()
        .filter(|row| row.table_name == table)
    {
        let definition = crate::catalog::index::index_definition(row)
            .map_err(|error| super::ddl_storage_error("constraint index lock", error))?;
        if definition.relationships.owning_constraint == Some(owner.object_id) {
            identity = definition.catalog.map(|catalog| catalog.identity.object_id);
            break;
        }
    }
    let identity =
        identity.ok_or_else(|| SQLError::Internal("constraint has no owned index".into()))?;
    let current =
        crate::schema::indexes::registry::binding::index_identity(registry, identity, mode)
            .map_err(|error| super::ddl_storage_error("constraint index lock", error))?
            .ok_or_else(|| SQLError::Internal("constraint index disappeared".into()))?;
    *name = current.relation.name;
    Ok(())
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
            lock_alter_children(binding, primary, RelationLockMode::AccessShare, |parent| {
                tables
                    .hierarchy
                    .partitions
                    .catalog
                    .direct_hierarchy_children(parent)
            })?;
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
        AlterTableAction::ValidateConstraint { name } => {
            lock_validation_reference(binding, tables, primary, name)?;
        }
        _ => {}
    }
    Ok(())
}

fn lock_validation_reference<S: Clone + 'static>(
    binding: &TableAlterBindingContext<'_>,
    tables: &TableAlterContext<'_, S>,
    primary: &str,
    name: &str,
) -> Result<(), SQLError> {
    let (columns, constraints) =
        crate::schema::constraints::table_constraint_state(&tables.constraints, primary)?;
    let target = constraint_validation(primary, name, &columns, &constraints)?;
    if let (false, ConstraintValidationKind::ForeignKey { referenced_table }) =
        (target.validated, target.kind)
    {
        let identity = binding
            .catalog
            .relation_object_id(referenced_table)?
            .ok_or_else(|| SQLError::UnknownTable(referenced_table.to_string()))?;
        lock_relation_identity(
            binding.catalog,
            binding.locks,
            referenced_table.to_string(),
            identity,
            RelationLockMode::RowShare,
            false,
        )?
        .ok_or_else(|| SQLError::UnknownTable(referenced_table.to_string()))?;
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
    if matches!(
        action,
        AlterTableAction::AddKeyConstraint { .. }
            | AlterTableAction::DropConstraint { .. }
            | AlterTableAction::RenameColumn { .. }
            | AlterTableAction::DropColumn { .. }
    ) && context
        .hierarchy
        .partitions
        .catalog
        .try_table_hierarchy(parent)
        .map_err(|error| SQLError::Internal(error.to_string()))?
        .partition_spec
        .is_some()
    {
        return Ok(true);
    }
    // PostgreSQL prepares ADD CONSTRAINT by locking every inheritor before execution can merge a constraint or stop its propagation with NO INHERIT.
    if matches!(
        action,
        AlterTableAction::AddCheckConstraint { .. }
            | AlterTableAction::AddNotNullConstraint { .. }
            | AlterTableAction::AddForeignKeyConstraint { .. }
    ) {
        return Ok(recurse);
    }
    if let AlterTableAction::ValidateConstraint { name } = action {
        let (columns, constraints) =
            crate::schema::constraints::table_constraint_state(&context.constraints, parent)?;
        return Ok(
            constraint_validation(parent, name, &columns, &constraints)?.requires_descendants()
        );
    }
    if let AlterTableAction::RenameConstraint { from: name, .. } = action {
        let (columns, constraints) =
            crate::schema::constraints::table_constraint_state(&context.constraints, parent)?;
        return Ok(
            uqa_sql::schema::constraint_changes::inheritance::InheritedConstraint::find(
                &columns,
                &constraints,
                name,
            )
            .is_some_and(|constraint| !constraint.no_inherit && recurse),
        );
    }
    Ok(false)
}
