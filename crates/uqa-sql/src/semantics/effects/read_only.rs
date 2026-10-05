//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL commands forbidden by read-only transactions and snapshot-setting rules.

use super::{command_payload_may_write_database, query_may_write_database, QueryEffectContext};
use crate::{
    ast::RelationPersistence,
    plan::{CommandPlan, UnifiedPlan},
    result::completion::command_tag_name,
    SQLError,
};

pub fn read_only_error(command: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "25006".into(),
        message: format!("cannot execute {command} in a read-only transaction"),
    }
}

fn table_is_temporary(
    context: &QueryEffectContext<'_>,
    table: &str,
) -> Result<Option<bool>, SQLError> {
    context
        .catalog
        .table_persistence(table)
        .map(|persistence| persistence.map(|value| value == RelationPersistence::Temporary))
        .map_err(|error| SQLError::Internal(format!("resolve read-only target `{table}`: {error}")))
}

fn dml_command(
    context: &QueryEffectContext<'_>,
    table: &str,
    command: &CommandPlan,
) -> Result<Option<&'static str>, SQLError> {
    match table_is_temporary(context, table)? {
        Some(false) => Ok(Some(command_tag_name(command))),
        Some(true) if command_payload_may_write_database(context, command)? => {
            Ok(Some(command_tag_name(command)))
        }
        Some(true) | None => Ok(None),
    }
}

/// The tag of a command a read-only transaction rejects, or `None` when the command may run: `ClassifyUtilityCommandAsReadOnly` rejects every utility command that changes persistent state, and `ExecCheckXactReadOnly` rejects a query that writes to a permanent relation.
pub fn forbidden_command(
    context: &QueryEffectContext<'_>,
    plan: &UnifiedPlan,
) -> Result<Option<&'static str>, SQLError> {
    let UnifiedPlan::Command(command) = plan else {
        let UnifiedPlan::Query(query) = plan else {
            unreachable!();
        };
        return query_may_write_database(context, query).map(|mutates| mutates.then_some("SELECT"));
    };
    let command = command.as_ref();
    match command {
        CommandPlan::Insert(insert) => dml_command(context, &insert.table, command),
        CommandPlan::Update(update) => dml_command(context, &update.table, command),
        CommandPlan::Delete(delete) => dml_command(context, &delete.table, command),
        CommandPlan::Merge(merge) => dml_command(context, &merge.target, command),
        CommandPlan::DeclareCursor { query, .. } => {
            query_may_write_database(context, query).map(|mutates| mutates.then_some("SELECT"))
        }
        CommandPlan::Execute { name, .. } => context
            .catalog
            .lookup_prepared(name)
            .map_or(Ok(None), |prepared| forbidden_command(context, &prepared)),
        CommandPlan::Explain {
            analyze: true,
            body,
            ..
        } => forbidden_command(context, body),
        // VACUUM's transaction-block prohibition has precedence over read-only validation and is enforced by its executor.
        CommandPlan::Analyze { .. }
        | CommandPlan::LockTable(_)
        | CommandPlan::Vacuum(_)
        | CommandPlan::SetVariable { .. }
        | CommandPlan::Notify { .. }
        | CommandPlan::Listen { .. }
        | CommandPlan::Unlisten { .. }
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
        CommandPlan::CreateTable(_)
        | CommandPlan::CreateTableIfNotExists(_)
        | CommandPlan::CreateTableAs { .. }
        | CommandPlan::CreateIndex(_)
        | CommandPlan::RenameIndex(_)
        | CommandPlan::Drop(_)
        | CommandPlan::AlterTable(_)
        | CommandPlan::AlterForeignTable(_)
        | CommandPlan::AlterView(_)
        | CommandPlan::CreateView { .. }
        | CommandPlan::CreateMaterializedView { .. }
        | CommandPlan::RefreshMaterializedView { .. }
        | CommandPlan::CreateSchema { .. }
        | CommandPlan::AlterSchemaOwner { .. }
        | CommandPlan::Truncate { .. }
        | CommandPlan::CreateSequence(_)
        | CommandPlan::AlterSequence(_)
        | CommandPlan::CreateDomain(_)
        | CommandPlan::CreateEnum(_)
        | CommandPlan::CreateCompositeType(_)
        | CommandPlan::AlterEnum(_)
        | CommandPlan::AlterTypeObject(_)
        | CommandPlan::GrantType(_)
        | CommandPlan::CreateForeignServer(_)
        | CommandPlan::CreateForeignTable(_)
        | CommandPlan::CreateForeignTableIfNotExists(_)
        | CommandPlan::CreateFunction(_)
        | CommandPlan::DropFunction(_)
        | CommandPlan::AlterRoutine(_)
        | CommandPlan::AlterRoutineOwner(_)
        | CommandPlan::RenameRoutine(_)
        | CommandPlan::GrantRoutine(_)
        | CommandPlan::GrantTable(_)
        | CommandPlan::GrantSequence(_)
        | CommandPlan::GrantDatabase(_)
        | CommandPlan::GrantSchema(_)
        | CommandPlan::GrantRole(_)
        | CommandPlan::CreateRole(_)
        | CommandPlan::AlterRole(_)
        | CommandPlan::RenameRole(_)
        | CommandPlan::DropRole(_)
        | CommandPlan::CreateTrigger(_)
        | CommandPlan::DropTrigger(_)
        | CommandPlan::CreateRule(_)
        | CommandPlan::DropRule(_) => Ok(Some(command_tag_name(command))),
    }
}

pub fn plan_sets_transaction_snapshot(plan: &UnifiedPlan) -> bool {
    !matches!(
        plan,
        UnifiedPlan::Command(command)
            if matches!(
                command.as_ref(),
                CommandPlan::SetVariable { .. }
                    | CommandPlan::Notify { .. }
                    | CommandPlan::Listen { .. }
                    | CommandPlan::Unlisten { .. }
                    | CommandPlan::ResetVariable { .. }
                    | CommandPlan::ResetAllVariables
                    | CommandPlan::SetConstraints { .. }
                    | CommandPlan::ShowVariable { .. }
                    | CommandPlan::Transaction(_)
                    | CommandPlan::LockTable(_)
                    | CommandPlan::FetchCursor(_)
                    | CommandPlan::CloseCursor { .. }
                    | CommandPlan::Deallocate { .. }
                    | CommandPlan::Load { .. }
                    | CommandPlan::Discard { .. }
            )
    )
}
