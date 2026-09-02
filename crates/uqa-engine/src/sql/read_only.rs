//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    mutability::{command_payload_may_write_database, query_may_write_database},
    Engine, SQLError,
};
use uqa_planner::{CommandPlan, UnifiedPlan};
use uqa_sql::ast::{DropKind, RelationPersistence};

fn read_only_error(command: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "25006".into(),
        message: format!("cannot execute {command} in a read-only transaction"),
    }
}

fn table_is_temporary(engine: &Engine, table: &str) -> Result<Option<bool>, SQLError> {
    engine
        .table_persistence(table)
        .map(|persistence| persistence.map(|value| value == RelationPersistence::Temporary))
        .map_err(|error| SQLError::Internal(format!("resolve read-only target `{table}`: {error}")))
}

fn dml_command<'a>(
    engine: &Engine,
    table: &str,
    command_name: &'a str,
    command: &CommandPlan,
) -> Result<Option<&'a str>, SQLError> {
    match table_is_temporary(engine, table)? {
        Some(false) => Ok(Some(command_name)),
        Some(true) if command_payload_may_write_database(engine, command)? => {
            Ok(Some(command_name))
        }
        Some(true) | None => Ok(None),
    }
}

fn forbidden_command(
    engine: &Engine,
    plan: &UnifiedPlan,
) -> Result<Option<&'static str>, SQLError> {
    let UnifiedPlan::Command(command) = plan else {
        let UnifiedPlan::Query(query) = plan else {
            unreachable!();
        };
        return query_may_write_database(engine, query).map(|mutates| mutates.then_some("SELECT"));
    };
    match command.as_ref() {
        CommandPlan::CreateTable(_) => Ok(Some("CREATE TABLE")),
        CommandPlan::CreateIndex(_) => Ok(Some("CREATE INDEX")),
        CommandPlan::Insert(insert) => dml_command(engine, &insert.table, "INSERT", command),
        CommandPlan::Update(update) => dml_command(engine, &update.table, "UPDATE", command),
        CommandPlan::Delete(delete) => dml_command(engine, &delete.table, "DELETE", command),
        CommandPlan::Merge(merge) => dml_command(engine, &merge.target, "MERGE", command),
        CommandPlan::Drop(drop) if drop.kind == DropKind::Table => Ok(Some("DROP TABLE")),
        CommandPlan::Drop(drop) if drop.kind == DropKind::Sequence => Ok(Some("DROP SEQUENCE")),
        CommandPlan::Drop(_) => Ok(Some("DROP")),
        CommandPlan::AlterTable(_) => Ok(Some("ALTER TABLE")),
        CommandPlan::AlterViewOptions(_) => Ok(Some("ALTER VIEW")),
        CommandPlan::CreateView { .. } => Ok(Some("CREATE VIEW")),
        CommandPlan::CreateMaterializedView { .. } => Ok(Some("CREATE MATERIALIZED VIEW")),
        CommandPlan::RefreshMaterializedView { .. } => Ok(Some("REFRESH MATERIALIZED VIEW")),
        CommandPlan::CreateSchema { .. } => Ok(Some("CREATE SCHEMA")),
        CommandPlan::Analyze { .. } => Ok(None),
        // VACUUM's transaction-block prohibition has precedence over read-only validation and is enforced by its executor.
        CommandPlan::Vacuum(_) => Ok(None),
        CommandPlan::Truncate { .. } => Ok(Some("TRUNCATE")),
        CommandPlan::CreateSequence(_) => Ok(Some("CREATE SEQUENCE")),
        CommandPlan::AlterSequence(_) => Ok(Some("ALTER SEQUENCE")),
        CommandPlan::CreateTableAs { .. } => Ok(Some("CREATE TABLE AS")),
        CommandPlan::DeclareCursor { query, .. } => {
            query_may_write_database(engine, query).map(|mutates| mutates.then_some("SELECT"))
        }
        CommandPlan::Execute { name, .. } => engine
            .lookup_prepared(name)
            .map_or(Ok(None), |prepared| forbidden_command(engine, &prepared)),
        CommandPlan::Explain {
            analyze: true,
            body,
            ..
        } => forbidden_command(engine, body),
        CommandPlan::CreateForeignServer(_) => Ok(Some("CREATE SERVER")),
        CommandPlan::CreateForeignTable(_) => Ok(Some("CREATE FOREIGN TABLE")),
        CommandPlan::CreateFunction(function) => Ok(Some(if function.is_procedure {
            "CREATE PROCEDURE"
        } else {
            "CREATE FUNCTION"
        })),
        CommandPlan::DropFunction(function) => Ok(Some(if function.is_procedure {
            "DROP PROCEDURE"
        } else {
            "DROP FUNCTION"
        })),
        CommandPlan::AlterRoutine(_) => Ok(Some("ALTER ROUTINE")),
        CommandPlan::AlterRoutineOwner(_) => Ok(Some("ALTER ROUTINE")),
        CommandPlan::GrantRoutine(_) => Ok(Some("GRANT ON ROUTINE")),
        CommandPlan::GrantSequence(_) => Ok(Some("GRANT ON SEQUENCE")),
        CommandPlan::GrantSchema(_) => Ok(Some("GRANT ON SCHEMA")),
        CommandPlan::GrantRole(_) => Ok(Some("GRANT ROLE")),
        CommandPlan::CreateRole(_) => Ok(Some("CREATE ROLE")),
        CommandPlan::AlterRole(_) => Ok(Some("ALTER ROLE")),
        CommandPlan::DropRole(_) => Ok(Some("DROP ROLE")),
        CommandPlan::CreateTrigger(_) => Ok(Some("CREATE TRIGGER")),
        CommandPlan::DropTrigger(_) => Ok(Some("DROP TRIGGER")),
        CommandPlan::CreateRule(_) => Ok(Some("CREATE RULE")),
        CommandPlan::DropRule(_) => Ok(Some("DROP RULE")),
        CommandPlan::SetVariable { .. }
        | CommandPlan::ResetVariable { .. }
        | CommandPlan::ResetAllVariables
        | CommandPlan::SetConstraints { .. }
        | CommandPlan::ShowVariable { .. }
        | CommandPlan::Discard { .. }
        | CommandPlan::Load { .. }
        | CommandPlan::Transaction(_)
        | CommandPlan::FetchCursor(_)
        | CommandPlan::CloseCursor { .. }
        | CommandPlan::Prepare { .. }
        | CommandPlan::Deallocate { .. }
        | CommandPlan::Explain { analyze: false, .. }
        | CommandPlan::DoBlock { .. }
        | CommandPlan::Call { .. } => Ok(None),
    }
}

pub(super) fn plan_sets_transaction_snapshot(plan: &UnifiedPlan) -> bool {
    !matches!(
        plan,
        UnifiedPlan::Command(command)
            if matches!(
                command.as_ref(),
                CommandPlan::SetVariable { .. }
                    | CommandPlan::ResetVariable { .. }
                    | CommandPlan::ResetAllVariables
                    | CommandPlan::SetConstraints { .. }
                    | CommandPlan::ShowVariable { .. }
                    | CommandPlan::Transaction(_)
                    | CommandPlan::FetchCursor(_)
                    | CommandPlan::CloseCursor { .. }
                    | CommandPlan::Deallocate { .. }
                    | CommandPlan::Load { .. }
                    | CommandPlan::Discard { .. }
            )
    )
}

pub(super) fn validate_transaction_plan(
    engine: &Engine,
    plan: &UnifiedPlan,
) -> Result<(), SQLError> {
    if engine.current_transaction_is_read_only() {
        if let Some(command) = forbidden_command(engine, plan)? {
            return Err(read_only_error(command));
        }
    }
    if plan_sets_transaction_snapshot(plan) {
        engine.mark_transaction_snapshot_set();
    }
    Ok(())
}
