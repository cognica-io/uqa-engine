//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute inheritance recursion while preserving child validation and CHECK propagation order.
use super::{
    ddl_storage_error, run_alter_table_action, AlterTableAction, AlterTableStmt, SQLError,
    TableAlterContext,
};
use std::collections::BTreeSet;
pub(super) fn run_recursive_alter_action<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    stmt: AlterTableStmt,
    action: AlterTableAction,
) -> Result<(), SQLError> {
    run_alter_action_branch(context, stmt, action, false, None, &mut BTreeSet::new())
}

fn run_alter_action_branch<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
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
    context.constraints.access.ensure_table_owner(&table)?;
    if recursing {
        uqa_sql::schema::table_alteration::normalize_inherited_action(&mut action);
    }
    if recursing && merge_existing_recursive_action(context, &table, &action)? {
        visiting.remove(&table);
        return Ok(());
    }
    let children = recursive_alter_children(context, &table, stmt.recurse, &action)?;
    let if_exists = stmt.if_exists;
    run_alter_table_action(
        context,
        stmt,
        action.clone(),
        recursing,
        inherited_not_null_name,
    )?;
    let child_not_null_name = if let AlterTableAction::SetNotNull { name } = &action {
        context
            .hierarchy
            .catalog
            .try_describe_table(&table)
            .map_err(|error| ddl_storage_error("ALTER TABLE SET NOT NULL", error))?
            .and_then(|columns| columns.into_iter().find(|column| column.name == *name))
            .and_then(|column| column.not_null_name)
    } else {
        None
    };
    for child in children {
        let qualifier = uqa_core::RelationIdentity::from_legacy_name(&child)
            .map_err(|error| {
                SQLError::Internal(format!("resolve recursive ALTER target: {error}"))
            })?
            .name;
        run_alter_action_branch(
            context,
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

fn recursive_alter_children<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
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
        AlterTableAction::AddColumn { column, .. } => context
            .addition
            .state
            .has_column(table, &column.name)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?,
        AlterTableAction::AddCheckConstraint { constraint } => context
            .hierarchy
            .catalog
            .try_check_constraint_definitions(table)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
            .iter()
            .any(|check| check.name.is_some() && check.name == constraint.name),
        AlterTableAction::AddNotNullConstraint { column, .. }
        | AlterTableAction::SetNotNull { name: column } => context
            .hierarchy
            .catalog
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
        return context
            .hierarchy
            .partitions
            .catalog
            .direct_hierarchy_children(table);
    }
    let requires_children = matches!(
        action,
        AlterTableAction::AddColumn { .. }
            | AlterTableAction::AddCheckConstraint { .. }
            | AlterTableAction::AddNotNullConstraint { .. }
    );
    if requires_children
        && !context
            .hierarchy
            .partitions
            .catalog
            .direct_hierarchy_children(table)?
            .is_empty()
    {
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

pub(super) fn materialize_recursive_action_names<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
    recurse: bool,
    action: &mut AlterTableAction,
) -> Result<(), SQLError> {
    if !recurse
        || context
            .hierarchy
            .partitions
            .catalog
            .direct_hierarchy_children(table)?
            .is_empty()
    {
        return Ok(());
    }
    let mut columns = context
        .hierarchy
        .catalog
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE recursive name binding", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut constraints = context
        .hierarchy
        .catalog
        .try_declared_table_constraints(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE recursive name binding", error))?;
    let relation = uqa_core::RelationIdentity::from_legacy_name(table)
        .map_err(|error| SQLError::Internal(format!("resolve ALTER TABLE relation: {error}")))?;
    let mut allocate = context.hierarchy.publication.allocate_identity;
    uqa_sql::schema::table_alteration::materialize_recursive_action_names(
        &relation,
        &mut columns,
        &mut constraints,
        action,
        &mut allocate,
    )
}

pub(super) fn merge_existing_recursive_action<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
    action: &AlterTableAction,
) -> Result<bool, SQLError> {
    match action {
        AlterTableAction::AddColumn { column, .. } => {
            let Some(mut columns) = context
                .hierarchy
                .catalog
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
            uqa_sql::schema::inheritance::merge_same_column(&mut merged, local)?;
            if let Some((name, validated, no_inherit)) = existing_not_null {
                merged.not_null_name = name;
                merged.not_null_validated = validated;
                merged.not_null_no_inherit = no_inherit;
            }
            columns[index] = merged;
            let constraints = context
                .hierarchy
                .catalog
                .try_declared_table_constraints(table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?;
            crate::schema::publication::hierarchy::replace_hierarchy_components(
                &context.hierarchy.publication,
                context.hierarchy.catalog,
                table,
                crate::schema::publication::hierarchy::HierarchySchemaChange {
                    columns,
                    checks: constraints.checks,
                    foreign_keys: constraints.foreign_keys,
                    key_constraints: constraints.key_constraints,
                    hierarchy: constraints.hierarchy,
                },
            )
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?;
            if needs_not_null_validation {
                crate::schema::constraints::validate_not_null_rows(
                    &context.constraints,
                    table,
                    &column.name,
                )?;
            }
            Ok(true)
        }
        AlterTableAction::AddCheckConstraint { constraint } => {
            crate::schema::constraints::checks::merge_added_check(
                &context.constraints,
                table,
                constraint.clone(),
            )
        }
        AlterTableAction::AddNotNullConstraint { column, .. }
        | AlterTableAction::SetNotNull { name: column } => {
            let definition = context
                .hierarchy
                .catalog
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
            uqa_sql::schema::constraint_changes::ensure_not_null_inheritable(
                table,
                &definition,
                sqlstate,
            )?;
            // An inherited merge preserves the existing child's validation state.
            Ok(true)
        }
        _ => Ok(false),
    }
}
