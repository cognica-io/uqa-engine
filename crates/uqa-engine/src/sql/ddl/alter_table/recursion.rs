//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` inheritance recursion for `ALTER TABLE` actions.

use super::{validate_all_table_rows, AlterTableAction, Engine, SQLError};
use crate::sql::ddl::ddl_storage_error;

pub(super) fn recursive_alter_targets(
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
        return Ok(vec![table.to_string()]);
    }
    if recurse {
        return engine.hierarchy_scan_tables(table, true);
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
    Ok(vec![table.to_string()])
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
            crate::engine_table_storage::materialize_constraint_names(
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
            crate::engine_table_storage::materialize_constraint_names(
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
                crate::engine_table_storage::materialize_constraint_names(
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
            let mut merged = column.clone();
            super::super::hierarchy::merge_same_column(&mut merged, local)?;
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
            validate_all_table_rows(engine)?;
            Ok(true)
        }
        AlterTableAction::AddCheckConstraint { constraint } => {
            let checks = engine
                .try_check_constraint_definitions(table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
            let Some(existing) = checks
                .iter()
                .find(|existing| existing.name == constraint.name)
            else {
                return Ok(false);
            };
            let same_expression = serde_json::to_value(&existing.expr)
                .map_err(|error| SQLError::Internal(format!("serialize CHECK: {error}")))?
                == serde_json::to_value(&constraint.expr)
                    .map_err(|error| SQLError::Internal(format!("serialize CHECK: {error}")))?;
            Ok(same_expression
                && existing.enforced == constraint.enforced
                && existing.no_inherit == constraint.no_inherit)
        }
        AlterTableAction::AddNotNullConstraint { column, .. }
        | AlterTableAction::SetNotNull { name: column } => Ok(engine
            .try_describe_table(table)
            .map_err(|error| ddl_storage_error("ALTER TABLE SET NOT NULL", error))?
            .and_then(|columns| {
                columns
                    .into_iter()
                    .find(|definition| definition.name == *column)
            })
            .is_some_and(|definition| definition.not_null)),
        _ => Ok(false),
    }
}
