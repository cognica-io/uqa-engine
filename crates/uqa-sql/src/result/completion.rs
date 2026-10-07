//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command completion owned by the plan that executed, including delegated EXECUTE.

use crate::ast::{
    AlterRoutineKind, AlterTableAction, AlterViewKind, DiscardTarget, DropKind, TransactionStmt,
};
use crate::plan::{CommandPlan, UnifiedPlan};
use crate::SQLResult;

pub fn set_command_completion(
    plan: &UnifiedPlan,
    result: &mut SQLResult,
    transaction_failed: bool,
) {
    let command = match plan {
        UnifiedPlan::Query(_) => {
            result.command_tag = Some(format!("SELECT {}", result.rows.len()));
            return;
        }
        UnifiedPlan::Command(command) => command.as_ref(),
    };
    if matches!(
        command,
        CommandPlan::Execute { .. }
            | CommandPlan::CreateTableAs { .. }
            | CommandPlan::CreateMaterializedView { .. }
    ) {
        // Delegated execution and optional population own their completion.
        return;
    }
    result.command_tag = Some(command_completion(command, result, transaction_failed));
}

fn command_completion(
    command: &CommandPlan,
    result: &SQLResult,
    transaction_failed: bool,
) -> String {
    match command {
        CommandPlan::Insert(_) => format!("INSERT 0 {}", result.affected_rows),
        CommandPlan::Update(_) => format!("UPDATE {}", result.affected_rows),
        CommandPlan::Delete(_) => format!("DELETE {}", result.affected_rows),
        CommandPlan::Merge(_) => format!("MERGE {}", result.affected_rows),
        CommandPlan::FetchCursor(fetch) if fetch.move_only => {
            format!("MOVE {}", result.affected_rows)
        }
        CommandPlan::FetchCursor(_) => format!("FETCH {}", result.rows.len()),
        CommandPlan::CreateTableAs { .. } | CommandPlan::CreateMaterializedView { .. } => {
            unreachable!("population execution owns completion")
        }
        CommandPlan::Execute { .. } => {
            unreachable!("EXECUTE preserves delegated command completion")
        }
        CommandPlan::Transaction(statement) => {
            transaction_completion(statement, transaction_failed).to_string()
        }
        command => command_tag_name(command).to_string(),
    }
}

/// The tag of a command without its row count, as `CreateCommandName` names a command in diagnostics such as the read-only transaction error.
#[expect(clippy::too_many_lines, reason = "exhaustive SQL command tag mapping")]
pub fn command_tag_name(command: &CommandPlan) -> &'static str {
    match command {
        CommandPlan::Insert(_) => "INSERT",
        CommandPlan::Update(_) => "UPDATE",
        CommandPlan::Delete(_) => "DELETE",
        CommandPlan::Merge(_) => "MERGE",
        CommandPlan::FetchCursor(fetch) => {
            if fetch.move_only {
                "MOVE"
            } else {
                "FETCH"
            }
        }
        CommandPlan::CreateTableAs {
            select_into: true, ..
        } => "SELECT INTO",
        CommandPlan::CreateTableAs { .. } => "CREATE TABLE AS",
        CommandPlan::CreateMaterializedView { .. } => "CREATE MATERIALIZED VIEW",
        CommandPlan::Execute { .. } => "EXECUTE",
        CommandPlan::Transaction(statement) => transaction_completion(statement, false),
        CommandPlan::CreateTable(_) | CommandPlan::CreateTableIfNotExists(_) => "CREATE TABLE",
        CommandPlan::CreateIndex(_) => "CREATE INDEX",
        CommandPlan::RenameIndex(_) => "ALTER INDEX",
        CommandPlan::Drop(statement) => match statement.kind {
            DropKind::Table => "DROP TABLE",
            DropKind::ForeignTable => "DROP FOREIGN TABLE",
            DropKind::ForeignServer => "DROP SERVER",
            DropKind::Index => "DROP INDEX",
            DropKind::View => "DROP VIEW",
            DropKind::MaterializedView => "DROP MATERIALIZED VIEW",
            DropKind::Schema => "DROP SCHEMA",
            DropKind::Sequence => "DROP SEQUENCE",
            DropKind::Domain => "DROP DOMAIN",
            DropKind::Type => "DROP TYPE",
        },
        CommandPlan::AlterTable(statement) => match statement.actions.as_slice() {
            [AlterTableAction::RenameTrigger { .. }] => "ALTER TRIGGER",
            [AlterTableAction::RenameRule { .. }] => "ALTER RULE",
            _ => "ALTER TABLE",
        },
        CommandPlan::AlterForeignTable(_) => "ALTER FOREIGN TABLE",
        CommandPlan::AlterView(statement) => match statement.kind {
            AlterViewKind::View => "ALTER VIEW",
            AlterViewKind::MaterializedView => "ALTER MATERIALIZED VIEW",
        },
        CommandPlan::CreateView { .. } => "CREATE VIEW",
        CommandPlan::RefreshMaterializedView { .. } => "REFRESH MATERIALIZED VIEW",
        CommandPlan::CreateSchema { .. } => "CREATE SCHEMA",
        CommandPlan::AlterSchemaOwner { .. } | CommandPlan::RenameSchema { .. } => "ALTER SCHEMA",
        CommandPlan::Notify { .. } => "NOTIFY",
        CommandPlan::Listen { .. } => "LISTEN",
        CommandPlan::Unlisten { .. } => "UNLISTEN",
        CommandPlan::SetVariable { .. } => "SET",
        CommandPlan::ResetVariable { .. } | CommandPlan::ResetAllVariables => "RESET",
        CommandPlan::SetConstraints { .. } => "SET CONSTRAINTS",
        CommandPlan::ShowVariable { .. } => "SHOW",
        CommandPlan::Discard { target } => match target {
            DiscardTarget::All => "DISCARD ALL",
            DiscardTarget::Plans => "DISCARD PLANS",
            DiscardTarget::Sequences => "DISCARD SEQUENCES",
            DiscardTarget::Temp => "DISCARD TEMP",
        },
        CommandPlan::Load { .. } => "LOAD",
        CommandPlan::Explain { .. } => "EXPLAIN",
        CommandPlan::Analyze { .. } => "ANALYZE",
        CommandPlan::LockTable(_) => "LOCK TABLE",
        CommandPlan::Vacuum(_) => "VACUUM",
        CommandPlan::Truncate { .. } => "TRUNCATE TABLE",
        CommandPlan::DeclareCursor { .. } => "DECLARE CURSOR",
        CommandPlan::CloseCursor { name } => {
            if name.is_some() {
                "CLOSE CURSOR"
            } else {
                "CLOSE CURSOR ALL"
            }
        }
        CommandPlan::CreateSequence(_) => "CREATE SEQUENCE",
        CommandPlan::CreateDomain(_) => "CREATE DOMAIN",
        CommandPlan::AlterDomain(_) => "ALTER DOMAIN",
        CommandPlan::CreateEnum(_) | CommandPlan::CreateCompositeType(_) => "CREATE TYPE",
        CommandPlan::AlterEnum(_) => "ALTER TYPE",
        CommandPlan::AlterTypeObject(statement) => match statement.kind {
            crate::ast::TypeObjectKind::Type => "ALTER TYPE",
            crate::ast::TypeObjectKind::Domain => "ALTER DOMAIN",
        },
        CommandPlan::GrantType(statement) => grant_completion(statement.is_grant),
        CommandPlan::AlterSequence(_) => "ALTER SEQUENCE",
        CommandPlan::Prepare { .. } => "PREPARE",
        CommandPlan::Deallocate { name } => {
            if name.is_some() {
                "DEALLOCATE"
            } else {
                "DEALLOCATE ALL"
            }
        }
        CommandPlan::CreateForeignWrapper(_) => "CREATE FOREIGN DATA WRAPPER",
        CommandPlan::CreateForeignServer(_) => "CREATE SERVER",
        CommandPlan::CreateForeignTable(_) | CommandPlan::CreateForeignTableDefinition(_) => {
            "CREATE FOREIGN TABLE"
        }
        CommandPlan::CreateFunction(statement) => {
            if statement.is_procedure {
                "CREATE PROCEDURE"
            } else {
                "CREATE FUNCTION"
            }
        }
        CommandPlan::DropFunction(statement) => {
            if statement.is_procedure {
                "DROP PROCEDURE"
            } else {
                "DROP FUNCTION"
            }
        }
        CommandPlan::AlterRoutine(statement) => alter_routine_completion(statement.kind),
        CommandPlan::AlterRoutineOwner(statement) => alter_routine_completion(statement.kind),
        CommandPlan::RenameRoutine(statement) => alter_routine_completion(statement.kind),
        CommandPlan::GrantRoutine(statement) => grant_completion(statement.is_grant),
        CommandPlan::GrantTable(statement) => grant_completion(statement.is_grant),
        CommandPlan::GrantSequence(statement) => grant_completion(statement.is_grant),
        CommandPlan::GrantDatabase(statement) => grant_completion(statement.is_grant),
        CommandPlan::GrantSchema(statement) => grant_completion(statement.is_grant),
        CommandPlan::GrantRole(statement) => {
            if statement.is_grant {
                "GRANT ROLE"
            } else {
                "REVOKE ROLE"
            }
        }
        CommandPlan::CreateRole(_) => "CREATE ROLE",
        CommandPlan::AlterRole(_) | CommandPlan::RenameRole(_) => "ALTER ROLE",
        CommandPlan::DropRole(_) => "DROP ROLE",
        CommandPlan::CreateTrigger(_) => "CREATE TRIGGER",
        CommandPlan::DropTrigger(_) => "DROP TRIGGER",
        CommandPlan::CreateRule(_) => "CREATE RULE",
        CommandPlan::DropRule(_) => "DROP RULE",
        CommandPlan::DoBlock { .. } => "DO",
        CommandPlan::Call { .. } => "CALL",
    }
}

pub const fn transaction_completion(statement: &TransactionStmt, failed: bool) -> &'static str {
    match statement {
        TransactionStmt::Begin | TransactionStmt::BeginWithCharacteristics(_) => "BEGIN",
        TransactionStmt::Commit | TransactionStmt::CommitAndChain => {
            if failed {
                "ROLLBACK"
            } else {
                "COMMIT"
            }
        }
        TransactionStmt::Rollback
        | TransactionStmt::RollbackAndChain
        | TransactionStmt::RollbackToSavepoint(_) => "ROLLBACK",
        TransactionStmt::Savepoint(_) => "SAVEPOINT",
        TransactionStmt::ReleaseSavepoint(_) => "RELEASE",
        TransactionStmt::SetCharacteristics(_)
        | TransactionStmt::SetSessionCharacteristics(_)
        | TransactionStmt::SetSnapshot(_) => "SET",
    }
}

const fn alter_routine_completion(kind: AlterRoutineKind) -> &'static str {
    match kind {
        AlterRoutineKind::Function => "ALTER FUNCTION",
        AlterRoutineKind::Procedure => "ALTER PROCEDURE",
        AlterRoutineKind::Routine => "ALTER ROUTINE",
    }
}

const fn grant_completion(is_grant: bool) -> &'static str {
    if is_grant {
        "GRANT"
    } else {
        "REVOKE"
    }
}
