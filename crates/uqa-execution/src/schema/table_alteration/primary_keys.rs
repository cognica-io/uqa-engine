//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schedule the inherited NOT NULL constraints required by a primary key.

use super::{
    ddl_storage_error, locking, prepare_alter_action, run_recursive_alter_action, TableAlterContext,
};
use crate::row_locks::RelationLockMode;
use uqa_sql::{
    ast::{AlterTableAction, AlterTableStmt, ColumnDef, TableKeyConstraint},
    schema::keys,
    SQLError,
};

pub(super) fn prepare<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
    columns: &[ColumnDef],
    key: &TableKeyConstraint,
    recurse: bool,
    mode: RelationLockMode,
) -> Result<Vec<String>, SQLError> {
    let mut required = Vec::new();
    let mut children = None;
    for name in &key.columns {
        if let Some(column) = columns
            .iter()
            .find(|column| column.name == *name && column.not_null)
        {
            keys::validate_primary_key_not_null_constraint(table, column)?;
            continue;
        }
        if !recurse {
            if children.is_none() {
                children = Some(locking::lock_direct_children(context, table, mode)?);
            }
            for child in children.as_ref().into_iter().flatten() {
                let columns = context
                    .hierarchy
                    .catalog
                    .try_describe_table(child)
                    .map_err(|error| ddl_storage_error("prepare primary-key NOT NULL", error))?
                    .ok_or_else(|| SQLError::UnknownTable(child.clone()))?;
                keys::validate_primary_key_not_null_child(child, &columns, name)?;
            }
        }
        if required.is_empty() {
            let mut action = not_null_requirement(name);
            locking::prepare_table_alter_action(
                &context.binding,
                context,
                table,
                true,
                &mut action,
                mode,
            )?;
        }
        required.push(name.clone());
    }
    let mut action = AlterTableAction::AddKeyConstraint {
        constraint: key.clone(),
    };
    locking::prepare_table_alter_action(
        &context.binding,
        context,
        table,
        recurse,
        &mut action,
        mode,
    )?;
    Ok(required)
}

pub(super) fn add_requirements<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    table: &str,
    qualifier: &str,
    key: &TableKeyConstraint,
    required: &[String],
    mode: RelationLockMode,
) -> Result<(), SQLError> {
    let columns = context
        .hierarchy
        .catalog
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("primary-key NOT NULL", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    keys::validate_added_key_declaration(&columns, key)?;
    for name in required {
        keys::validate_primary_key_not_null_target(table, &columns, name)?;
        let mut action = not_null_requirement(name);
        prepare_alter_action(context, table, true, &mut action, mode)?;
        run_recursive_alter_action(
            context,
            AlterTableStmt {
                table: table.into(),
                qualifier: qualifier.into(),
                if_exists: false,
                recurse: true,
                actions: Vec::new(),
            },
            action,
        )?;
    }
    Ok(())
}

fn not_null_requirement(name: &str) -> AlterTableAction {
    AlterTableAction::AddNotNullConstraint {
        name: None,
        column: name.into(),
        validated: true,
        no_inherit: false,
    }
}

pub(super) fn validate_rows<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    checks: Vec<(String, std::collections::BTreeSet<String>)>,
) -> Result<(), SQLError> {
    for (table, required) in checks {
        let columns = context
            .hierarchy
            .catalog
            .try_describe_table(&table)
            .map_err(|error| ddl_storage_error("validate primary-key NOT NULL", error))?
            .ok_or_else(|| SQLError::UnknownTable(table.clone()))?;
        let selected = columns
            .iter()
            .filter(|column| column.not_null && required.contains(&column.name))
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>();
        if !selected.is_empty() {
            crate::schema::validation::validate_not_null_columns(
                context.constraints.rows.reads,
                context.constraints.rows.memory,
                context.constraints.rows.cancellation,
                &table,
                &selected,
            )?;
        }
    }
    Ok(())
}
