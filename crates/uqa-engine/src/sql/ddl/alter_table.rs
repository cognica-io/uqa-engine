//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ALTER TABLE schema mutation and existing-row backfill.

use super::{AlterTableAction, AlterTableStmt, Engine, SQLError, SQLResult};

mod constraint_drop;
pub(crate) use constraint_drop::{drop_column_cascade, drop_constraint_dependency};

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
    engine.with_implicit_transaction(move |engine| {
        uqa_execution::schema::table_alteration::run_alter_table(
            &engine.table_alter_context(),
            stmt,
        )
    })
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
