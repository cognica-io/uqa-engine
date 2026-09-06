//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` inheritance recursion for `ALTER TABLE` actions.

use super::{run_alter_table_action, AlterTableAction, AlterTableStmt, Engine, SQLError};
use crate::sql::ddl::ddl_storage_error;
use std::collections::BTreeSet;

pub(super) fn run_recursive_alter_action(
    engine: &Engine,
    stmt: AlterTableStmt,
    action: AlterTableAction,
) -> Result<(), SQLError> {
    run_alter_action_branch(engine, stmt, action, false, None, &mut BTreeSet::new())
}

fn run_alter_action_branch(
    engine: &Engine,
    stmt: AlterTableStmt,
    mut action: AlterTableAction,
    recursing: bool,
    inherited_not_null_name: Option<String>,
    visiting: &mut BTreeSet<String>,
) -> Result<(), SQLError> {
    let table = stmt.table.clone();
    if !visiting.insert(table.clone()) {
        return Err(SQLError::Internal(format!(
            "table inheritance cycle reaches `{table}`"
        )));
    }
    engine.ensure_table_owner(&table)?;
    if recursing {
        if let AlterTableAction::AddColumn { column, .. } = &mut action {
            column.not_null_is_local = !column.not_null;
            column.check_is_local = column.check.is_none();
            column.check_object_id = None;
            if column.check_no_inherit {
                column.check = None;
                column.check_name = None;
                column.check_is_local = true;
                column.check_no_inherit = false;
            }
        }
        if let AlterTableAction::AddCheckConstraint { constraint } = &mut action {
            constraint.is_local = false;
            constraint.object_id = None;
        }
    }
    if recursing && merge_existing_recursive_action(engine, &table, &action)? {
        visiting.remove(&table);
        return Ok(());
    }
    let children = recursive_alter_children(engine, &table, stmt.recurse, &action)?;
    let if_exists = stmt.if_exists;
    run_alter_table_action(
        engine,
        stmt,
        action.clone(),
        recursing,
        inherited_not_null_name,
    )?;
    let child_not_null_name = if let AlterTableAction::SetNotNull { name } = &action {
        engine
            .try_describe_table(&table)
            .map_err(|error| ddl_storage_error("ALTER TABLE SET NOT NULL", error))?
            .and_then(|columns| columns.into_iter().find(|column| column.name == *name))
            .and_then(|column| column.not_null_name)
    } else {
        None
    };
    for child in children {
        let qualifier = crate::RelationIdentity::from_legacy_name(&child)
            .map_err(|error| {
                SQLError::Internal(format!("resolve recursive ALTER target: {error}"))
            })?
            .name;
        run_alter_action_branch(
            engine,
            AlterTableStmt {
                table: child,
                qualifier,
                if_exists,
                recurse: true,
                actions: Vec::new(),
            },
            action.clone(),
            true,
            child_not_null_name.clone(),
            visiting,
        )?;
    }
    visiting.remove(&table);
    Ok(())
}

fn recursive_alter_children(
    engine: &Engine,
    table: &str,
    recurse: bool,
    action: &AlterTableAction,
) -> Result<Vec<String>, SQLError> {
    let recursive = matches!(action, AlterTableAction::AddColumn { .. })
        || matches!(action, AlterTableAction::AddCheckConstraint { constraint } if !constraint.no_inherit)
        || matches!(
            action,
            AlterTableAction::AddNotNullConstraint {
                no_inherit: false,
                ..
            }
        )
        || matches!(action, AlterTableAction::SetNotNull { .. });
    if !recursive {
        return Ok(Vec::new());
    }
    let unchanged = match action {
        AlterTableAction::AddColumn { column, .. } => engine
            .try_table_has_column(table, &column.name)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?,
        AlterTableAction::AddCheckConstraint { constraint } => engine
            .try_check_constraint_definitions(table)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
            .iter()
            .any(|check| check.name.is_some() && check.name == constraint.name),
        AlterTableAction::AddNotNullConstraint { column, .. }
        | AlterTableAction::SetNotNull { name: column } => engine
            .try_describe_table(table)
            .map_err(|error| ddl_storage_error("ALTER TABLE SET NOT NULL", error))?
            .and_then(|columns| {
                columns
                    .into_iter()
                    .find(|definition| definition.name == *column)
            })
            .is_some_and(|definition| definition.not_null && definition.not_null_validated),
        _ => false,
    };
    if unchanged {
        return Ok(Vec::new());
    }
    if recurse {
        return engine.direct_hierarchy_children(table);
    }
    let requires_children = matches!(
        action,
        AlterTableAction::AddColumn { .. }
            | AlterTableAction::AddCheckConstraint { .. }
            | AlterTableAction::AddNotNullConstraint { .. }
    );
    if requires_children && !engine.direct_hierarchy_children(table)?.is_empty() {
        let object = if matches!(action, AlterTableAction::AddColumn { .. }) {
            "column"
        } else {
            "constraint"
        };
        return Err(SQLError::Routine {
            sqlstate: "42P16".into(),
            message: format!("{object} must be added to child tables too"),
        });
    }
    Ok(Vec::new())
}

pub(super) fn materialize_recursive_action_names(
    engine: &Engine,
    table: &str,
    recurse: bool,
    action: &mut AlterTableAction,
) -> Result<(), SQLError> {
    if !recurse || engine.direct_hierarchy_children(table)?.is_empty() {
        return Ok(());
    }
    let mut columns = engine
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE recursive name binding", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut constraints = engine
        .try_declared_table_constraints(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE recursive name binding", error))?;
    let relation = crate::RelationIdentity::from_legacy_name(table)
        .map_err(|error| SQLError::Internal(format!("resolve ALTER TABLE relation: {error}")))?;
    match action {
        AlterTableAction::AddColumn { column, .. } => {
            columns.push(column.clone());
            crate::engine_table_storage::materialize_constraint_metadata(
                &relation,
                &mut columns,
                &mut constraints,
            )
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?;
            *column = columns
                .pop()
                .ok_or_else(|| SQLError::Internal("new column disappeared".into()))?;
        }
        AlterTableAction::AddCheckConstraint { constraint }
            if !constraint.no_inherit && constraint.name.is_none() =>
        {
            constraints.checks.push(constraint.clone());
            crate::engine_table_storage::materialize_constraint_metadata(
                &relation,
                &mut columns,
                &mut constraints,
            )
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
            *constraint = constraints
                .checks
                .pop()
                .ok_or_else(|| SQLError::Internal("new CHECK constraint disappeared".into()))?;
        }
        AlterTableAction::AddNotNullConstraint {
            name,
            column,
            validated,
            no_inherit: false,
        } if name.is_none() => {
            if let Some(definition) = columns
                .iter_mut()
                .find(|definition| definition.name == *column && !definition.not_null)
            {
                definition.not_null = true;
                definition.not_null_explicit = true;
                definition.not_null_validated = *validated;
                crate::engine_table_storage::materialize_constraint_metadata(
                    &relation,
                    &mut columns,
                    &mut constraints,
                )
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
                *name = columns
                    .iter()
                    .find(|definition| definition.name == *column)
                    .and_then(|definition| definition.not_null_name.clone());
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn merge_existing_recursive_action(
    engine: &Engine,
    table: &str,
    action: &AlterTableAction,
) -> Result<bool, SQLError> {
    match action {
        AlterTableAction::AddColumn { column, .. } => {
            let Some(mut columns) = engine
                .try_describe_table(table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?
            else {
                return Err(SQLError::UnknownTable(table.to_string()));
            };
            let Some(index) = columns
                .iter()
                .position(|definition| definition.name == column.name)
            else {
                return Ok(false);
            };
            let local = columns[index].clone();
            let needs_not_null_validation = column.not_null && !local.not_null;
            let existing_not_null = local.not_null.then(|| {
                (
                    local.not_null_name.clone(),
                    local.not_null_validated,
                    local.not_null_no_inherit,
                )
            });
            let mut merged = column.clone();
            super::super::hierarchy::merge_same_column(&mut merged, local)?;
            if let Some((name, validated, no_inherit)) = existing_not_null {
                merged.not_null_name = name;
                merged.not_null_validated = validated;
                merged.not_null_no_inherit = no_inherit;
            }
            columns[index] = merged;
            let constraints = engine
                .try_declared_table_constraints(table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?;
            engine
                .replace_table_hierarchy_components(
                    table,
                    columns,
                    constraints.checks,
                    constraints.foreign_keys,
                    constraints.key_constraints,
                    constraints.hierarchy,
                )
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?;
            if needs_not_null_validation {
                super::constraint_lifecycle::validate_not_null_rows(engine, table, &column.name)?;
            }
            Ok(true)
        }
        AlterTableAction::AddCheckConstraint { constraint } => {
            super::checks::merge_added_check(engine, table, constraint.clone())
        }
        AlterTableAction::AddNotNullConstraint { column, .. }
        | AlterTableAction::SetNotNull { name: column } => {
            let definition = engine
                .try_describe_table(table)
                .map_err(|error| ddl_storage_error("ALTER TABLE SET NOT NULL", error))?
                .and_then(|columns| {
                    columns
                        .into_iter()
                        .find(|definition| definition.name == *column)
                });
            let Some(definition) = definition.filter(|definition| definition.not_null) else {
                return Ok(false);
            };
            let sqlstate = if matches!(action, AlterTableAction::SetNotNull { .. }) {
                "0A000"
            } else {
                "55000"
            };
            super::constraint_lifecycle::ensure_not_null_inheritable(table, &definition, sqlstate)?;
            // An inherited merge preserves the existing child's validation state.
            Ok(true)
        }
        _ => Ok(false),
    }
}
