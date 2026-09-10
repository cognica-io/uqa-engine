//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ALTER TABLE schema mutation and existing-row backfill.

use super::{ddl_storage_error, AlterTableAction, AlterTableStmt, Engine, SQLError, SQLResult};

mod checks;
mod constraint_drop;
mod constraint_lifecycle;
mod recursion;

use constraint_drop::{drop_column, drop_constraint};
pub(crate) use constraint_drop::{drop_column_cascade, drop_constraint_dependency};
use constraint_lifecycle::{
    add_check_constraint, add_foreign_key_constraint, add_not_null_constraint, alter_constraint,
    set_not_null_constraint, validate_and_mark_constraint,
};
use constraint_lifecycle::{constraint_error, table_constraint_state};
use recursion::{materialize_recursive_action_names, run_recursive_alter_action};

pub(in crate::sql) fn run_alter_table(
    engine: &Engine,
    mut stmt: AlterTableStmt,
) -> Result<SQLResult, SQLError> {
    if engine.in_transaction_block()
        && stmt.actions.iter().any(|action| {
            matches!(
                action,
                AlterTableAction::DetachPartition {
                    concurrently: true,
                    ..
                }
            )
        })
    {
        return Err(SQLError::Routine {
            sqlstate: "25001".into(),
            message: "ALTER TABLE ... DETACH CONCURRENTLY cannot run inside a transaction block"
                .into(),
        });
    }
    let resolution = if matches!(
        stmt.actions.as_slice(),
        [AlterTableAction::RenameTable { .. }]
    ) {
        let Some(resolution) =
            engine.resolve_relation_rename_source(&stmt.table, stmt.if_exists)?
        else {
            return Ok(SQLResult::empty());
        };
        Some(resolution)
    } else {
        engine.try_resolve_visible_relation_kind(&stmt.table)?
    };
    match resolution {
        Some((canonical, "table")) => stmt.table = canonical,
        Some((canonical, "sequence")) => {
            return run_alter_sequence_with_table_syntax(engine, canonical, &stmt);
        }
        Some((canonical, "foreign table")) => {
            return run_alter_foreign_table_with_table_syntax(engine, canonical, &stmt);
        }
        Some((canonical, kind @ ("view" | "materialized view"))) => {
            return run_alter_view_with_table_syntax(engine, canonical, kind, &stmt);
        }
        Some((canonical, kind)) => {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("ALTER TABLE: relation `{canonical}` is a {kind}, not a table"),
            });
        }
        None if stmt.if_exists => {
            engine.push_sql_notice(
                "NOTICE",
                &format!("relation \"{}\" does not exist, skipping", stmt.table),
            );
            return Ok(SQLResult::empty());
        }
        None => {
            return Err(SQLError::Unsupported(format!(
                "ALTER TABLE: relation `{}` does not exist",
                stmt.table
            )));
        }
    }
    engine.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::AccessExclusive,
    )?;
    engine.with_implicit_transaction(move |engine| run_alter_table_inner(engine, stmt))
}

fn run_alter_view_with_table_syntax(
    engine: &Engine,
    canonical: String,
    kind: &str,
    stmt: &AlterTableStmt,
) -> Result<SQLResult, SQLError> {
    if let [AlterTableAction::RenameTable { to }] = stmt.actions.as_slice() {
        engine.alter_view(&uqa_sql::ast::AlterViewStmt {
            name: canonical,
            kind: if kind == "view" {
                uqa_sql::ast::AlterViewKind::View
            } else {
                uqa_sql::ast::AlterViewKind::MaterializedView
            },
            if_exists: stmt.if_exists,
            action: uqa_sql::ast::AlterViewAction::RenameTo(to.clone()),
        })?;
        return Ok(SQLResult::empty());
    }
    if kind == "view"
        && stmt.actions.iter().all(|action| {
            matches!(
                action,
                AlterTableAction::RenameRule { .. } | AlterTableAction::RenameTrigger { .. }
            )
        })
    {
        return engine.with_implicit_transaction(|engine| {
            for action in &stmt.actions {
                match action {
                    AlterTableAction::RenameRule { from, to } => {
                        engine.rename_rule(&canonical, from, to)?;
                    }
                    AlterTableAction::RenameTrigger { from, to } => {
                        engine.rename_trigger(&canonical, from, to)?;
                    }
                    _ => unreachable!("view ALTER was restricted to event lifecycle actions"),
                }
            }
            Ok(SQLResult::empty())
        });
    }
    Err(SQLError::Routine {
        sqlstate: "42809".into(),
        message: format!("ALTER TABLE: relation `{canonical}` is a {kind}, not a table"),
    })
}

fn run_alter_foreign_table_with_table_syntax(
    engine: &Engine,
    canonical: String,
    stmt: &AlterTableStmt,
) -> Result<SQLResult, SQLError> {
    if stmt.actions.iter().all(|action| {
        matches!(
            action,
            AlterTableAction::RenameTrigger { .. } | AlterTableAction::SetTriggerEnableMode { .. }
        )
    }) {
        return engine.with_implicit_transaction(|engine| {
            engine.ensure_foreign_table_owner(&canonical)?;
            for action in &stmt.actions {
                match action {
                    AlterTableAction::RenameTrigger { from, to } => {
                        engine.rename_trigger(&canonical, from, to)?;
                    }
                    AlterTableAction::SetTriggerEnableMode { name, mode, .. } => {
                        engine.set_trigger_enable_mode(&canonical, name.as_deref(), *mode)?;
                    }
                    _ => unreachable!("foreign-table trigger actions were checked above"),
                }
            }
            Ok(SQLResult::empty())
        });
    }
    let action = match stmt.actions.as_slice() {
        [AlterTableAction::ChangeOwner { owner }] => {
            uqa_sql::ast::AlterForeignTableAction::OwnerTo(owner.clone())
        }
        [AlterTableAction::RenameTable { to }] => {
            uqa_sql::ast::AlterForeignTableAction::RenameTo(to.clone())
        }
        _ => {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!(
                    "ALTER TABLE: relation `{canonical}` is a foreign table, not a table"
                ),
            });
        }
    };
    engine.alter_foreign_table(&uqa_sql::ast::AlterForeignTableStmt {
        name: canonical,
        if_exists: stmt.if_exists,
        action,
    })?;
    Ok(SQLResult::empty())
}

fn run_alter_sequence_with_table_syntax(
    engine: &Engine,
    canonical: String,
    stmt: &AlterTableStmt,
) -> Result<SQLResult, SQLError> {
    let mut alter = uqa_sql::ast::AlterSequence {
        name: canonical.clone(),
        if_exists: stmt.if_exists,
        ..uqa_sql::ast::AlterSequence::default()
    };
    match stmt.actions.as_slice() {
        [AlterTableAction::SetPersistence { persistence }] => {
            alter.persistence = Some(*persistence);
        }
        [AlterTableAction::RenameTable { to }] => {
            alter.lifecycle = uqa_sql::ast::SequenceLifecycle::RenameTo { name: to.clone() };
        }
        [AlterTableAction::SetSchema { schema }] => {
            alter.lifecycle = uqa_sql::ast::SequenceLifecycle::SetSchema {
                schema: schema.clone(),
            };
        }
        [AlterTableAction::ChangeOwner { owner }] => {
            alter.role_owner = Some(owner.clone());
        }
        _ => {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("ALTER TABLE: relation `{canonical}` is a sequence, not a table"),
            });
        }
    }
    super::run_alter_sequence(engine, alter)
}

fn run_alter_table_inner(engine: &Engine, stmt: AlterTableStmt) -> Result<SQLResult, SQLError> {
    let AlterTableStmt {
        table,
        qualifier,
        if_exists,
        recurse,
        actions,
    } = stmt;
    engine.ensure_table_owner(&table)?;
    for mut action in actions {
        if let AlterTableAction::AddColumn {
            column,
            if_not_exists: true,
        } = &action
        {
            if engine
                .try_table_has_column(&table, &column.name)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?
            {
                engine.push_sql_notice(
                    "NOTICE",
                    &format!(
                        "column \"{}\" of relation \"{qualifier}\" already exists, skipping",
                        column.name
                    ),
                );
                continue;
            }
        }
        match &mut action {
            AlterTableAction::AddColumn { column, .. } => {
                column.ty = crate::sql::resolve_declared_column_type(engine, &column.ty)?;
            }
            AlterTableAction::AlterColumnType { ty, .. } => {
                *ty = crate::sql::resolve_declared_column_type(engine, ty)?;
            }
            _ => {}
        }
        materialize_recursive_action_names(engine, &table, recurse, &mut action)?;
        // Column merging can stop at an existing child column. Its CHECK still has an independent inheritance lifecycle and must reach every supplying edge.
        let column_check = if let AlterTableAction::AddColumn { column, .. } = &mut action {
            checks::take_column_check(column)
        } else {
            None
        };
        run_recursive_alter_action(
            engine,
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
            materialize_recursive_action_names(engine, &table, recurse, &mut action)?;
            run_recursive_alter_action(
                engine,
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
fn run_alter_table_action(
    engine: &Engine,
    stmt: AlterTableStmt,
    action: AlterTableAction,
    recursing: bool,
    inherited_not_null_name: Option<String>,
) -> Result<(), SQLError> {
    if matches!(&action, AlterTableAction::AddKeyConstraint { .. }) {
        let persistence = engine
            .table_persistence(&stmt.table)
            .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
        if persistence == Some(uqa_sql::ast::RelationPersistence::Temporary) {
            engine.ensure_temporary_relation_creation_privilege()?;
        } else {
            engine.ensure_existing_relation_creation_privilege(&stmt.table)?;
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
        engine.ensure_no_pending_trigger_events(&stmt.table, "ALTER TABLE")?;
    }
    match action {
        AlterTableAction::ChangeOwner { owner } => {
            engine.alter_table_role_owner(&stmt.table, &owner)?;
        }
        action @ (AlterTableAction::AddInheritance { .. }
        | AlterTableAction::DropInheritance { .. }
        | AlterTableAction::AttachPartition { .. }
        | AlterTableAction::DetachPartition { .. }) => {
            uqa_execution::schema::hierarchy::run_alter_hierarchy_action(
                &engine.hierarchy_execution_context(),
                &stmt.table,
                action,
            )?;
        }
        AlterTableAction::AddColumn {
            column,
            if_not_exists,
        } => {
            uqa_execution::schema::columns::addition::add_column(
                &engine.column_addition_context(),
                &stmt.table,
                &stmt.qualifier,
                column,
                if_not_exists,
            )?;
        }
        AlterTableAction::AddKeyConstraint { constraint } => {
            uqa_execution::schema::constraints::add_key_constraint(
                &engine.constraint_alter_context(),
                &stmt.table,
                &stmt.qualifier,
                constraint,
            )?;
        }
        AlterTableAction::AddCheckConstraint { constraint } => {
            add_check_constraint(engine, &stmt.table, &stmt.qualifier, constraint)?;
        }
        AlterTableAction::AddForeignKeyConstraint { constraint } => {
            add_foreign_key_constraint(engine, &stmt.table, &stmt.qualifier, constraint)?;
        }
        AlterTableAction::AddNotNullConstraint {
            name,
            column,
            validated,
            no_inherit,
        } => {
            add_not_null_constraint(
                engine,
                &stmt.table,
                name,
                &column,
                validated,
                no_inherit,
                !recursing,
            )?;
        }
        AlterTableAction::ValidateConstraint { name } => {
            if !checks::validate_check(engine, &stmt.table, &name, stmt.recurse)? {
                validate_and_mark_constraint(engine, &stmt.table, &name)?;
            }
        }
        AlterTableAction::AlterConstraint {
            name,
            enforceability,
            deferrability,
            no_inherit,
        } => {
            alter_constraint(
                engine,
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
            drop_constraint(engine, &stmt.table, &name, if_exists, cascade, stmt.recurse)?;
        }
        AlterTableAction::DropColumn {
            name,
            if_exists,
            cascade,
        } => {
            drop_column(engine, &stmt.table, &name, if_exists, cascade)?;
        }
        AlterTableAction::RenameColumn { from, to } => {
            super::validate_postgres_column_name(&to)?;
            if !engine
                .try_table_has_column(&stmt.table, &from)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME COLUMN", err))?
            {
                return Err(SQLError::Routine {
                    sqlstate: "42703".into(),
                    message: format!("column \"{from}\" does not exist"),
                });
            }
            if engine
                .try_table_has_column(&stmt.table, &to)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME COLUMN", err))?
            {
                let relation = crate::RelationIdentity::from_legacy_name(&stmt.table)
                    .map_err(SQLError::Internal)?;
                return Err(SQLError::Routine {
                    sqlstate: "42701".into(),
                    message: format!(
                        "column \"{to}\" of relation \"{}\" already exists",
                        relation.name
                    ),
                });
            }
            engine
                .try_rename_column(&stmt.table, &from, &to)
                .map_err(|e| ddl_storage_error("ALTER TABLE RENAME COLUMN", e))?;
        }
        AlterTableAction::RenameTable { to } => {
            if engine
                .try_has_table(&to)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: relation `{to}` already exists"
                )));
            }
            if !engine
                .try_rename_table(&stmt.table, &to)
                .map_err(|e| ddl_storage_error("ALTER TABLE RENAME", e))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME: rename of `{}` failed",
                    stmt.table
                )));
            }
        }
        AlterTableAction::RenameTrigger { from, to } => {
            engine.rename_trigger(&stmt.table, &from, &to)?;
        }
        AlterTableAction::RenameConstraint { from, to } => {
            if !checks::rename_check(engine, &stmt.table, &from, &to, stmt.recurse)? {
                engine.rename_trigger_constraint(&stmt.table, &from, &to)?;
            }
        }
        AlterTableAction::RenameRule { from, to } => {
            engine.rename_rule(&stmt.table, &from, &to)?;
        }
        AlterTableAction::SetTriggerEnableMode {
            name,
            user_only: _,
            mode,
        } => {
            engine.set_trigger_enable_mode(&stmt.table, name.as_deref(), mode)?;
        }
        AlterTableAction::SetRuleEnableMode { name, mode } => {
            engine.set_rule_enable_mode(&stmt.table, &name, mode)?;
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
            uqa_execution::schema::columns::alteration::set_default(
                &engine.column_alter_context(),
                &stmt.table,
                &name,
                default,
            )?;
        }
        AlterTableAction::DropDefault { name } => {
            uqa_execution::schema::columns::alteration::drop_default(
                &engine.column_alter_context(),
                &stmt.table,
                &name,
            )?;
        }
        AlterTableAction::SetExpression { name, expression } => {
            uqa_execution::schema::columns::alteration::set_expression(
                &engine.column_alter_context(),
                &stmt.table,
                &stmt.qualifier,
                &name,
                expression,
            )?;
        }
        AlterTableAction::DropExpression { name } => {
            uqa_execution::schema::columns::alteration::drop_expression(
                &engine.column_alter_context(),
                &stmt.table,
                &name,
            )?;
        }
        AlterTableAction::SetNotNull { name } => {
            set_not_null_constraint(
                engine,
                &stmt.table,
                &name,
                stmt.recurse,
                !recursing,
                inherited_not_null_name,
            )?;
        }
        AlterTableAction::DropNotNull { name } => {
            let (columns, _) = table_constraint_state(engine, &stmt.table)?;
            let column = columns
                .iter()
                .find(|column| column.name == name)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            if let Some(constraint_name) = column.not_null_name.as_deref() {
                drop_constraint(
                    engine,
                    &stmt.table,
                    constraint_name,
                    false,
                    false,
                    stmt.recurse,
                )?;
            }
        }
        AlterTableAction::AlterColumnType { name, ty, using } => {
            uqa_execution::schema::columns::alteration::alter_type(
                &engine.column_alter_context(),
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
