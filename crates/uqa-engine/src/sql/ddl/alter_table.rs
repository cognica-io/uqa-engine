//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ALTER TABLE schema mutation and existing-row backfill.

use super::{AlterTableAction, AlterTableStmt, Engine, SQLError, SQLResult};

pub(in crate::sql) fn run_alter_table(
    engine: &Engine,
    mut stmt: AlterTableStmt,
) -> Result<SQLResult, SQLError> {
    uqa_sql::schema::table_alteration::syntax::validate_alter_table_transaction(
        &stmt,
        engine.in_transaction_block(),
    )?;
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
            return run_alter_sequence_with_table_syntax(engine, &canonical, &stmt);
        }
        Some((canonical, "foreign table")) => {
            return run_alter_foreign_table_with_table_syntax(engine, &canonical, &stmt);
        }
        Some((canonical, kind @ ("view" | "materialized view"))) => {
            return run_alter_view_with_table_syntax(engine, &canonical, kind, &stmt);
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
    canonical: &str,
    kind: &str,
    stmt: &AlterTableStmt,
) -> Result<SQLResult, SQLError> {
    if let Some(change) = uqa_sql::schema::table_alteration::syntax::alter_view_from_table_syntax(
        canonical, kind, stmt,
    )? {
        engine.alter_view(&change)?;
        return Ok(SQLResult::empty());
    }
    engine.with_implicit_transaction(|engine| {
        for action in &stmt.actions {
            match action {
                AlterTableAction::RenameRule { from, to } => {
                    engine.rename_rule(canonical, from, to)?;
                }
                AlterTableAction::RenameTrigger { from, to } => {
                    engine.rename_trigger(canonical, from, to)?;
                }
                _ => unreachable!("view ALTER was restricted to event lifecycle actions"),
            }
        }
        Ok(SQLResult::empty())
    })
}

fn run_alter_foreign_table_with_table_syntax(
    engine: &Engine,
    canonical: &str,
    stmt: &AlterTableStmt,
) -> Result<SQLResult, SQLError> {
    let Some(change) =
        uqa_sql::schema::table_alteration::syntax::alter_foreign_table_from_table_syntax(
            canonical, stmt,
        )?
    else {
        return engine.with_implicit_transaction(|engine| {
            engine.ensure_foreign_table_owner(canonical)?;
            for action in &stmt.actions {
                match action {
                    AlterTableAction::RenameTrigger { from, to } => {
                        engine.rename_trigger(canonical, from, to)?;
                    }
                    AlterTableAction::SetTriggerEnableMode { name, mode, .. } => {
                        engine.set_trigger_enable_mode(canonical, name.as_deref(), *mode)?;
                    }
                    _ => unreachable!("foreign-table trigger actions were checked above"),
                }
            }
            Ok(SQLResult::empty())
        });
    };
    engine.alter_foreign_table(&change)?;
    Ok(SQLResult::empty())
}

fn run_alter_sequence_with_table_syntax(
    engine: &Engine,
    canonical: &str,
    stmt: &AlterTableStmt,
) -> Result<SQLResult, SQLError> {
    let alter = uqa_sql::schema::table_alteration::syntax::alter_sequence_from_table_syntax(
        canonical, stmt,
    )?;
    super::run_alter_sequence(engine, alter)
}
