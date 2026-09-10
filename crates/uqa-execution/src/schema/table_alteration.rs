//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute table alterations in declaration order and propagate inheritable actions through child relations.
use crate::schema::columns::removal::drop_column;
use crate::schema::constraints::{
    add_check_constraint, add_foreign_key_constraint, add_not_null_constraint, alter_constraint,
    checks, drop::drop_constraint, set_not_null_constraint, table_constraint_state,
    validate_and_mark_constraint,
};
use uqa_sql::{
    ast::{AlterTableAction, AlterTableStmt},
    SQLError, SQLResult,
};
mod context;
pub mod entry;
mod recursion;
pub use context::*;
use recursion::{materialize_recursive_action_names, run_recursive_alter_action};
fn ddl_storage_error(action: &str, error: uqa_storage::StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, &error)
}
pub fn run_alter_table<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    stmt: AlterTableStmt,
) -> Result<SQLResult, SQLError> {
    let AlterTableStmt {
        table,
        qualifier,
        if_exists,
        recurse,
        actions,
    } = stmt;
    context.constraints.access.ensure_table_owner(&table)?;
    for mut action in actions {
        if let AlterTableAction::AddColumn {
            column,
            if_not_exists: true,
        } = &action
        {
            if context
                .addition
                .state
                .has_column(&table, &column.name)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?
            {
                context.constraints.notices.lock().push((
                    "NOTICE".into(),
                    format!(
                        "column \"{}\" of relation \"{qualifier}\" already exists, skipping",
                        column.name
                    ),
                ));
                continue;
            }
        }
        match &mut action {
            AlterTableAction::AddColumn { column, .. } => {
                column.ty = uqa_sql::type_resolution::resolve_declared_column_type(
                    context.hierarchy.publication.types,
                    &column.ty,
                )?;
            }
            AlterTableAction::AlterColumnType { ty, .. } => {
                *ty = uqa_sql::type_resolution::resolve_declared_column_type(
                    context.hierarchy.publication.types,
                    ty,
                )?;
            }
            _ => {}
        }
        materialize_recursive_action_names(context, &table, recurse, &mut action)?;
        // Column merging can stop at an existing child column. Its CHECK still has an independent inheritance lifecycle and must reach every supplying edge.
        let column_check = if let AlterTableAction::AddColumn { column, .. } = &mut action {
            uqa_sql::schema::constraint_changes::take_column_check(column)
        } else {
            None
        };
        run_recursive_alter_action(
            context,
            AlterTableStmt {
                table: table.clone(),
                qualifier: qualifier.clone(),
                if_exists,
                recurse,
                actions: Vec::new(),
            },
            action,
        )?;
        if let Some(constraint) = column_check {
            let mut action = AlterTableAction::AddCheckConstraint { constraint };
            materialize_recursive_action_names(context, &table, recurse, &mut action)?;
            run_recursive_alter_action(
                context,
                AlterTableStmt {
                    table: table.clone(),
                    qualifier: qualifier.clone(),
                    if_exists,
                    recurse,
                    actions: Vec::new(),
                },
                action,
            )?;
        }
    }
    Ok(SQLResult::empty())
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves DDL dependency and action order"
)]
fn run_alter_table_action<S: Clone + 'static>(
    context: &TableAlterContext<'_, S>,
    stmt: AlterTableStmt,
    action: AlterTableAction,
    recursing: bool,
    inherited_not_null_name: Option<String>,
) -> Result<(), SQLError> {
    if matches!(&action, AlterTableAction::AddKeyConstraint { .. }) {
        let persistence = context
            .hierarchy
            .catalog
            .table_persistence(&stmt.table)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
        if persistence == Some(uqa_sql::ast::RelationPersistence::Temporary) {
            context.addition.namespace.ensure_temporary_creation()?;
        } else {
            context
                .addition
                .namespace
                .ensure_existing_creation(&stmt.table)?;
        }
    }
    if !matches!(
        action,
        AlterTableAction::RenameColumn { .. }
            | AlterTableAction::RenameTable { .. }
            | AlterTableAction::RenameTrigger { .. }
            | AlterTableAction::RenameConstraint { .. }
            | AlterTableAction::RenameRule { .. }
    ) {
        context
            .constraints
            .access
            .ensure_no_pending_events(&stmt.table, "ALTER TABLE")?;
    }
    match action {
        AlterTableAction::ChangeOwner { owner } => {
            context.lifecycle.change_owner(&stmt.table, &owner)?;
        }
        action @ (AlterTableAction::AddInheritance { .. }
        | AlterTableAction::DropInheritance { .. }
        | AlterTableAction::AttachPartition { .. }
        | AlterTableAction::DetachPartition { .. }) => {
            crate::schema::hierarchy::run_alter_hierarchy_action(
                &context.hierarchy,
                &stmt.table,
                action,
            )?;
        }
        AlterTableAction::AddColumn {
            column,
            if_not_exists,
        } => {
            crate::schema::columns::addition::add_column(
                &context.addition,
                &stmt.table,
                &stmt.qualifier,
                column,
                if_not_exists,
            )?;
        }
        AlterTableAction::AddKeyConstraint { constraint } => {
            crate::schema::constraints::add_key_constraint(
                &context.constraints,
                &stmt.table,
                &stmt.qualifier,
                constraint,
            )?;
        }
        AlterTableAction::AddCheckConstraint { constraint } => {
            add_check_constraint(
                &context.constraints,
                &stmt.table,
                &stmt.qualifier,
                constraint,
            )?;
        }
        AlterTableAction::AddForeignKeyConstraint { constraint } => {
            add_foreign_key_constraint(
                &context.constraints,
                &stmt.table,
                &stmt.qualifier,
                constraint,
            )?;
        }
        AlterTableAction::AddNotNullConstraint {
            name,
            column,
            validated,
            no_inherit,
        } => {
            add_not_null_constraint(
                &context.constraints,
                &stmt.table,
                name,
                &column,
                validated,
                no_inherit,
                !recursing,
            )?;
        }
        AlterTableAction::ValidateConstraint { name } => {
            if !checks::validate_check(&context.constraints, &stmt.table, &name, stmt.recurse)? {
                validate_and_mark_constraint(&context.constraints, &stmt.table, &name)?;
            }
        }
        AlterTableAction::AlterConstraint {
            name,
            enforceability,
            deferrability,
            no_inherit,
        } => {
            alter_constraint(
                &context.constraints,
                &stmt.table,
                &name,
                enforceability,
                deferrability,
                no_inherit,
            )?;
        }
        AlterTableAction::DropConstraint {
            name,
            if_exists,
            cascade,
        } => {
            drop_constraint(
                &context.constraints,
                &stmt.table,
                &name,
                if_exists,
                cascade,
                stmt.recurse,
            )?;
        }
        AlterTableAction::DropColumn {
            name,
            if_exists,
            cascade,
        } => {
            drop_column(&context.removal, &stmt.table, &name, if_exists, cascade)?;
        }
        AlterTableAction::RenameColumn { from, to } => {
            uqa_sql::schema::columns::validate_postgres_column_name(&to)?;
            if !context
                .addition
                .state
                .has_column(&stmt.table, &from)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME COLUMN", err))?
            {
                return Err(SQLError::Routine {
                    sqlstate: "42703".into(),
                    message: format!("column \"{from}\" does not exist"),
                });
            }
            if context
                .addition
                .state
                .has_column(&stmt.table, &to)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME COLUMN", err))?
            {
                let relation = uqa_core::RelationIdentity::from_legacy_name(&stmt.table)
                    .map_err(SQLError::Internal)?;
                return Err(SQLError::Routine {
                    sqlstate: "42701".into(),
                    message: format!(
                        "column \"{to}\" of relation \"{}\" already exists",
                        relation.name
                    ),
                });
            }
            context
                .lifecycle
                .rename_column(&stmt.table, &from, &to)
                .map_err(|e| ddl_storage_error("ALTER TABLE RENAME COLUMN", e))?;
        }
        AlterTableAction::RenameTable { to } => {
            if context
                .lifecycle
                .has_table(&to)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: relation `{to}` already exists"
                )));
            }
            if !context
                .lifecycle
                .rename_table(&stmt.table, &to)
                .map_err(|e| ddl_storage_error("ALTER TABLE RENAME", e))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: rename of `{}` failed",
                    stmt.table
                )));
            }
        }
        AlterTableAction::RenameTrigger { from, to } => {
            context.events.rename_trigger(&stmt.table, &from, &to)?;
        }
        AlterTableAction::RenameConstraint { from, to } => {
            if !checks::rename_check(&context.constraints, &stmt.table, &from, &to, stmt.recurse)? {
                context
                    .events
                    .rename_trigger_constraint(&stmt.table, &from, &to)?;
            }
        }
        AlterTableAction::RenameRule { from, to } => {
            context.events.rename_rule(&stmt.table, &from, &to)?;
        }
        AlterTableAction::SetTriggerEnableMode {
            name,
            user_only: _,
            mode,
        } => {
            context
                .events
                .set_trigger_enable_mode(&stmt.table, name.as_deref(), mode)?;
        }
        AlterTableAction::SetRuleEnableMode { name, mode } => {
            context
                .events
                .set_rule_enable_mode(&stmt.table, &name, mode)?;
        }
        AlterTableAction::SetPersistence { persistence } => {
            return Err(SQLError::Unsupported(format!(
                "ALTER TABLE SET {} is not supported for tables",
                match persistence {
                    uqa_sql::ast::RelationPersistence::Permanent => "LOGGED",
                    uqa_sql::ast::RelationPersistence::Unlogged => "UNLOGGED",
                    uqa_sql::ast::RelationPersistence::Temporary => "TEMPORARY",
                }
            )));
        }
        AlterTableAction::SetSchema { schema } => {
            return Err(SQLError::Unsupported(format!(
                "ALTER TABLE SET SCHEMA {schema} is not supported for tables"
            )));
        }
        AlterTableAction::SetDefault { name, default } => {
            crate::schema::columns::alteration::set_default(
                &context.columns,
                &stmt.table,
                &name,
                default,
            )?;
        }
        AlterTableAction::DropDefault { name } => {
            crate::schema::columns::alteration::drop_default(&context.columns, &stmt.table, &name)?;
        }
        AlterTableAction::SetExpression { name, expression } => {
            crate::schema::columns::alteration::set_expression(
                &context.columns,
                &stmt.table,
                &stmt.qualifier,
                &name,
                expression,
            )?;
        }
        AlterTableAction::DropExpression { name } => {
            crate::schema::columns::alteration::drop_expression(
                &context.columns,
                &stmt.table,
                &name,
            )?;
        }
        AlterTableAction::SetNotNull { name } => {
            set_not_null_constraint(
                &context.constraints,
                &stmt.table,
                &name,
                stmt.recurse,
                !recursing,
                inherited_not_null_name,
            )?;
        }
        AlterTableAction::DropNotNull { name } => {
            let (columns, _) = table_constraint_state(&context.constraints, &stmt.table)?;
            let column = columns
                .iter()
                .find(|column| column.name == name)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            if let Some(constraint_name) = column.not_null_name.as_deref() {
                drop_constraint(
                    &context.constraints,
                    &stmt.table,
                    constraint_name,
                    false,
                    false,
                    stmt.recurse,
                )?;
            }
        }
        AlterTableAction::AlterColumnType { name, ty, using } => {
            crate::schema::columns::alteration::alter_type(
                &context.columns,
                &stmt.table,
                &stmt.qualifier,
                &name,
                &ty,
                using.as_ref(),
            )?;
        }
    }
    Ok(())
}
