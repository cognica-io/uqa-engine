//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ALTER TABLE schema mutation and existing-row backfill.

use super::{
    coerce_to_column_type, ddl_storage_error, eval_lowered_expression, index_vectors_for_type,
    rewrite_column_values_to_type, AlterTableAction, AlterTableStmt, BTreeMap, ColumnType,
    Document, Engine, RowUpdateVectors, SQLError, SQLResult, Value,
};
use uqa_sql::ast::{ForeignKey, GeneratedColumn, GeneratedColumnKind};

use super::constraint_validation::{
    resolve_foreign_key_parent, validate_foreign_key_definition as validate_temporal_foreign_key,
};
use super::defaults::validate_default_expression;
use super::hierarchy_alter::run_alter_hierarchy_action;

mod constraint_drop;
mod constraint_lifecycle;
mod foreign_key;
mod recursion;

pub(crate) use constraint_drop::drop_constraint_dependency;
use constraint_drop::{drop_column_cascade, drop_column_restrict, drop_constraint};
use constraint_lifecycle::{
    add_check_constraint, add_foreign_key_constraint, add_not_null_constraint, alter_constraint,
    ensure_constraint_name_available, validate_altered_constraint_column_types,
    validate_and_mark_constraint,
};
use constraint_lifecycle::{
    constraint_error, find_constraint, publish_constraint_state, table_constraint_state,
    ConstraintLocation,
};
pub(super) use foreign_key::{
    column_foreign_key, validate_bound_foreign_key_definition_with_local_state,
    validate_foreign_key_definition_with_local_state,
};
use recursion::{
    materialize_recursive_action_names, merge_existing_recursive_action, recursive_alter_targets,
};

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
        materialize_recursive_action_names(engine, &table, recurse, &mut action)?;
        let targets = recursive_alter_targets(engine, &table, recurse, &action)?;
        for target in targets {
            if target != table && merge_existing_recursive_action(engine, &target, &action)? {
                continue;
            }
            let target_qualifier = if target == table {
                qualifier.clone()
            } else {
                crate::RelationIdentity::from_legacy_name(&target)
                    .map_err(|error| {
                        SQLError::Internal(format!("resolve recursive ALTER target: {error}"))
                    })?
                    .name
            };
            run_alter_table_action(
                engine,
                AlterTableStmt {
                    table: target,
                    qualifier: target_qualifier,
                    if_exists,
                    recurse: false,
                    actions: Vec::new(),
                },
                action.clone(),
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
) -> Result<(), SQLError> {
    engine.ensure_table_owner(&stmt.table)?;
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
            run_alter_hierarchy_action(engine, &stmt.table, action)?;
        }
        AlterTableAction::AddColumn {
            mut column,
            if_not_exists,
        } => {
            super::validate_postgres_column_name(&column.name)?;
            super::validate_postgres_relation_column_type(&column.name, &column.ty)?;
            let col_name = column.name.clone();
            if engine
                .try_table_has_column(&stmt.table, &col_name)
                .map_err(|err| ddl_storage_error("ALTER TABLE ADD COLUMN", err))?
            {
                if if_not_exists {
                    return Ok(());
                }
                let relation =
                    crate::RelationIdentity::from_legacy_name(&stmt.table).map_err(|error| {
                        SQLError::Internal(format!("resolve ALTER TABLE target: {error}"))
                    })?;
                return Err(SQLError::Routine {
                    sqlstate: "42701".into(),
                    message: format!(
                        "column \"{col_name}\" of relation \"{}\" already exists",
                        relation.name
                    ),
                });
            }
            if let Some(default) = &mut column.default {
                validate_default_expression(engine, default, &column.ty)?;
            }
            let mut candidate_columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            candidate_columns.push(column.clone());
            let check_columns = candidate_columns.clone();
            if let Some(check) = &mut column.check {
                super::constraint_validation::validate_check_expression(
                    engine,
                    &stmt.table,
                    &stmt.qualifier,
                    &check_columns,
                    check,
                )?;
                crate::sql::reject_stored_regrole_constants(engine, check, None)?;
                candidate_columns
                    .last_mut()
                    .expect("new column candidate exists")
                    .check
                    .clone_from(&column.check);
            }
            let key_constraints = engine
                .try_key_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?;
            let foreign_keys = engine
                .try_foreign_keys(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?;
            crate::sql::generated::prepare_generated_columns(
                engine,
                &stmt.qualifier,
                &mut candidate_columns,
                &key_constraints,
                &foreign_keys,
            )?;
            column.generated = candidate_columns
                .last()
                .and_then(|candidate| candidate.generated.clone());
            if let Some(reference) = column.references.clone() {
                let mut foreign_key = column_foreign_key(&column, &reference);
                validate_foreign_key_definition_with_local_state(
                    engine,
                    &stmt.table,
                    Some(&candidate_columns),
                    None,
                    &mut foreign_key,
                )?;
                let [referenced_column] = foreign_key.ref_columns.as_slice() else {
                    return Err(SQLError::Internal(
                        "column FOREIGN KEY did not resolve exactly one referenced column".into(),
                    ));
                };
                let Some(reference) = column.references.as_mut() else {
                    return Err(SQLError::Internal(
                        "column FOREIGN KEY disappeared during validation".into(),
                    ));
                };
                reference.table = foreign_key.ref_table;
                reference.column = Some(referenced_column.clone());
            }
            if column.primary_key || column.unique {
                let persistence = engine.table_persistence(&stmt.table).map_err(|error| {
                    ddl_storage_error("ALTER TABLE ADD COLUMN constraint", error)
                })?;
                if persistence == Some(uqa_sql::ast::RelationPersistence::Temporary) {
                    engine.ensure_temporary_relation_creation_privilege()?;
                } else {
                    engine.ensure_existing_relation_creation_privilege(&stmt.table)?;
                }
            }
            let generated_kind = column.generated.as_ref().map(|generated| generated.kind);
            match column.ty {
                ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                    engine
                        .create_vector_field(&stmt.table, col_name.clone(), dim)
                        .map_err(|err| ddl_storage_error("ALTER TABLE vector field", err))?;
                }
                ColumnType::Text if generated_kind != Some(GeneratedColumnKind::Virtual) => {
                    if let Err(e) = engine.add_fts_field(&stmt.table, col_name.clone()) {
                        return Err(SQLError::Internal(format!("add_fts_field: {e}")));
                    }
                }
                _ => {}
            }
            // Capture the default expression and NOT NULL flag before
            // moving the column into the engine so we can backfill any
            // existing rows. PostgreSQL evaluates the default once per
            // existing row at ALTER TABLE time, which keeps NOT NULL
            // constraints satisfiable for non-empty tables.
            let column_not_null = column.not_null;
            engine
                .try_register_column(&stmt.table, column)
                .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN", e))?;
            if let Some(kind) = generated_kind {
                validate_and_rewrite_generated_rows(
                    engine,
                    &stmt.table,
                    kind == GeneratedColumnKind::Stored,
                )?;
            } else {
                let default_expr = engine
                    .try_column_default_expr(&stmt.table, &col_name)
                    .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN default", e))?;
                let missing_value = backfill_added_column(
                    engine,
                    &stmt.table,
                    &col_name,
                    default_expr.as_ref(),
                    column_not_null,
                )?;
                let table = engine.require_table(&stmt.table)?;
                let mut columns = table.columns.write();
                let definition = columns
                    .iter_mut()
                    .find(|definition| definition.name == col_name)
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "new column `{col_name}` disappeared during ALTER TABLE"
                        ))
                    })?;
                definition.missing_value = missing_value;
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ADD COLUMN", e))?;
        }
        AlterTableAction::AddKeyConstraint { mut constraint } => {
            let mut columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            let declared_constraints = engine
                .try_declared_table_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
            ensure_constraint_name_available(
                &columns,
                &declared_constraints,
                constraint.name.as_deref(),
                &stmt.table,
            )?;
            super::constraint_indexes::name_constraint_indexes(
                engine,
                &stmt.table,
                std::slice::from_mut(&mut constraint),
            )?;
            let mut key_constraints = engine
                .try_key_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
            key_constraints.push(constraint.clone());
            let foreign_keys = engine
                .try_foreign_keys(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
            crate::sql::generated::prepare_generated_columns(
                engine,
                &stmt.qualifier,
                &mut columns,
                &key_constraints,
                &foreign_keys,
            )?;
            validate_added_key_constraint(engine, &stmt.table, &constraint)?;
            engine
                .add_key_constraint(&stmt.table, &constraint)
                .map_err(|error| ddl_storage_error("ALTER TABLE ADD CONSTRAINT", error))?;
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
            add_not_null_constraint(engine, &stmt.table, name, &column, validated, no_inherit)?;
        }
        AlterTableAction::ValidateConstraint { name } => {
            validate_and_mark_constraint(engine, &stmt.table, &name)?;
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
            drop_constraint(engine, &stmt.table, &name, if_exists, cascade)?;
        }
        AlterTableAction::DropColumn {
            name,
            if_exists,
            cascade: false,
        } => {
            engine.handle_drop_column_event_dependencies(&stmt.table, &name, false)?;
            drop_column_restrict(engine, &stmt.table, &name, if_exists)?;
        }
        AlterTableAction::DropColumn {
            name,
            if_exists,
            cascade: true,
        } => {
            engine.handle_drop_column_event_dependencies(&stmt.table, &name, true)?;
            drop_column_cascade(engine, &stmt.table, &name, if_exists)?;
        }
        AlterTableAction::RenameColumn { from, to } => {
            super::validate_postgres_column_name(&to)?;
            if !engine
                .try_table_has_column(&stmt.table, &from)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME COLUMN", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME COLUMN: column `{from}` does not exist"
                )));
            }
            if engine
                .try_table_has_column(&stmt.table, &to)
                .map_err(|err| ddl_storage_error("ALTER TABLE RENAME COLUMN", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE RENAME COLUMN: column `{to}` already exists"
                )));
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
            engine.rename_trigger_constraint(&stmt.table, &from, &to)?;
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
        AlterTableAction::SetDefault { name, mut default } => {
            reject_default_change_on_generated_column(engine, &stmt.table, &name)?;
            let target = engine
                .column_type(&stmt.table, &name)
                .map_err(|error| ddl_storage_error("ALTER COLUMN SET DEFAULT", error))?
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            validate_default_expression(engine, &mut default, &target)?;
            if !engine
                .set_column_default(&stmt.table, &name, Some(default))
                .map_err(|err| ddl_storage_error("ALTER COLUMN SET DEFAULT", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
        AlterTableAction::DropDefault { name } => {
            reject_default_change_on_generated_column(engine, &stmt.table, &name)?;
            if !engine
                .set_column_default(&stmt.table, &name, None)
                .map_err(|err| ddl_storage_error("ALTER COLUMN DROP DEFAULT", err))?
            {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE ALTER COLUMN: column `{name}` does not exist"
                )));
            }
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
        AlterTableAction::SetExpression { name, expression } => {
            let mut columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            let column = columns
                .iter_mut()
                .find(|column| column.name == name)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            let Some(current) = column.generated.as_ref() else {
                return Err(SQLError::TypeMismatch(format!(
                    "column `{name}` of relation `{}` is not a generated column",
                    stmt.table
                )));
            };
            let kind = current.kind;
            let generated = GeneratedColumn {
                kind,
                expression: Box::new(expression),
                function_dependencies: Vec::new(),
            };
            column.generated = Some(generated.clone());
            let key_constraints = engine
                .try_key_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?;
            let foreign_keys = engine
                .try_foreign_keys(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?;
            crate::sql::generated::prepare_generated_columns(
                engine,
                &stmt.qualifier,
                &mut columns,
                &key_constraints,
                &foreign_keys,
            )?;
            let generated = columns
                .iter()
                .find(|column| column.name == name)
                .and_then(|column| column.generated.clone())
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "generated column `{name}` disappeared during validation"
                    ))
                })?;
            engine
                .set_column_generated(&stmt.table, &name, Some(generated))
                .map_err(|error| ddl_storage_error("ALTER COLUMN SET EXPRESSION", error))?;
            validate_and_rewrite_generated_rows(
                engine,
                &stmt.table,
                kind == GeneratedColumnKind::Stored,
            )?;
        }
        AlterTableAction::DropExpression { name } => {
            let columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN DROP EXPRESSION", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            let column = columns
                .iter()
                .find(|column| column.name == name)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            let Some(generated) = column.generated.as_ref() else {
                return Err(SQLError::TypeMismatch(format!(
                    "column `{name}` of relation `{}` is not a generated column",
                    stmt.table
                )));
            };
            if generated.kind == GeneratedColumnKind::Virtual {
                return Err(SQLError::Unsupported(format!(
                    "ALTER TABLE / DROP EXPRESSION is not supported for virtual generated column `{name}`"
                )));
            }
            engine
                .set_column_generated(&stmt.table, &name, None)
                .map_err(|error| ddl_storage_error("ALTER COLUMN DROP EXPRESSION", error))?;
        }
        AlterTableAction::SetNotNull { name } => {
            ensure_column_exists(engine, &stmt.table, &name)?;
            ensure_existing_values_not_null(engine, &stmt.table, &name)?;
            engine
                .set_column_not_null(&stmt.table, &name, true)
                .map_err(|err| ddl_storage_error("ALTER COLUMN SET NOT NULL", err))?;
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
        AlterTableAction::DropNotNull { name } => {
            let (columns, _) = table_constraint_state(engine, &stmt.table)?;
            let column = columns
                .iter()
                .find(|column| column.name == name)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            if let Some(constraint_name) = column.not_null_name.as_deref() {
                drop_constraint(engine, &stmt.table, constraint_name, false, false)?;
            }
        }
        AlterTableAction::AlterColumnType { name, ty, using } => {
            ensure_column_exists(engine, &stmt.table, &name)?;
            super::validate_postgres_relation_column_type(&name, &ty)?;
            let mut candidate_columns = engine
                .try_describe_table(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
                .ok_or_else(|| SQLError::UnknownTable(stmt.table.clone()))?;
            let candidate = candidate_columns
                .iter_mut()
                .find(|column| column.name == name)
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            candidate.ty.clone_from(&ty);
            let target_generated_kind =
                candidate.generated.as_ref().map(|generated| generated.kind);
            if target_generated_kind.is_none() {
                let dependents = engine
                    .generated_columns_referencing_column(&stmt.table, &name)
                    .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?;
                if !dependents.is_empty() {
                    return Err(SQLError::TypeMismatch(format!(
                        "cannot alter type of column `{name}` because generated column(s) `{}` depend on it",
                        dependents.join("`, `")
                    )));
                }
            }
            let key_constraints = engine
                .try_key_constraints(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?;
            let foreign_keys = engine
                .try_foreign_keys(&stmt.table)
                .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?;
            validate_altered_constraint_column_types(
                engine,
                &stmt.table,
                &candidate_columns,
                &key_constraints,
                &foreign_keys,
            )?;
            crate::sql::generated::prepare_generated_columns(
                engine,
                &stmt.qualifier,
                &mut candidate_columns,
                &key_constraints,
                &foreign_keys,
            )?;
            let old_ty = engine
                .column_type(&stmt.table, &name)
                .map_err(|err| ddl_storage_error("ALTER COLUMN TYPE", err))?
                .ok_or_else(|| SQLError::UnknownColumn(format!("{}.{name}", stmt.table)))?;
            let old_was_vector = matches!(&old_ty, ColumnType::Vector(_) | ColumnType::Tensor(_));
            let new_is_vector = matches!(&ty, ColumnType::Vector(_) | ColumnType::Tensor(_));

            // Row rewrites maintain every currently registered vector index.
            // Detach a vector/tensor index before converting its values to a
            // scalar type, otherwise the first converted scalar is fed back
            // into the old vector index. The enclosing ALTER transaction
            // restores both catalog and physical index state if conversion
            // of any row subsequently fails.
            if old_was_vector && !new_is_vector {
                engine
                    .try_drop_vector_indexes_for_column(&stmt.table, &name)
                    .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
            }
            if target_generated_kind.is_none() {
                rewrite_column_values_to_type(
                    engine,
                    &stmt.table,
                    &name,
                    &old_ty,
                    &ty,
                    using.as_ref(),
                )?;
            }
            engine
                .set_column_type(&stmt.table, &name, &ty)
                .map_err(|err| ddl_storage_error("ALTER COLUMN TYPE", err))?;
            match ty {
                ColumnType::Text if target_generated_kind != Some(GeneratedColumnKind::Virtual) => {
                    if let Err(e) = engine.add_fts_field(&stmt.table, name.clone()) {
                        return Err(SQLError::Internal(format!("add_fts_field: {e}")));
                    }
                }
                ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                    engine
                        .try_rebuild_vector_index_for_column(&stmt.table, &name, dim)
                        .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
                }
                _ => {}
            }
            if let Some(kind) = target_generated_kind {
                validate_and_rewrite_generated_rows(
                    engine,
                    &stmt.table,
                    kind == GeneratedColumnKind::Stored,
                )?;
            }
            validate_all_table_rows(engine)?;
            engine
                .try_persist_table_schema(&stmt.table)
                .map_err(|e| ddl_storage_error("ALTER TABLE ALTER COLUMN", e))?;
        }
    }
    Ok(())
}

mod validation;
use validation::{
    backfill_added_column, ensure_column_exists, ensure_existing_values_not_null,
    reject_default_change_on_generated_column, validate_added_key_constraint,
    validate_all_table_rows, validate_and_rewrite_generated_rows,
};
