//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Parser entry point and exhaustive statement-family dispatch.

use super::administrative::{
    compile_analyze, compile_explain, compile_transaction, compile_truncate, compile_variable_set,
    discard_target,
};
use super::dml::{compile_delete, compile_update};
use super::drop_alter::{compile_alter_table, compile_drop, compile_rename};
use super::merge::compile_merge;
use super::relations::{
    compile_create_foreign_server, compile_create_foreign_table, compile_create_schema,
    compile_create_table_as, compile_create_view, compile_deallocate, compile_execute,
    compile_prepare,
};
use super::routines::{compile_call, compile_create_function, compile_do};
use super::sequences::{compile_alter_sequence, compile_create_sequence};
use super::{
    compile_create_index, compile_create_table, compile_insert, compile_select,
    compile_values_lists, Node, NodeEnum, Result, SQLError, Statement,
};

pub fn compile(sql: &str) -> Result<Vec<Statement>> {
    let parsed = pg_query::parse(sql)?;
    let mut out = Vec::with_capacity(parsed.protobuf.stmts.len());
    for raw in parsed.protobuf.stmts {
        let node = raw
            .stmt
            .ok_or_else(|| SQLError::Internal("parser returned an empty statement".into()))?;
        out.push(compile_stmt(&node)?);
    }
    Ok(out)
}

pub(super) fn compile_stmt(node: &Node) -> Result<Statement> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Unsupported("empty statement".into()));
    };
    match inner {
        NodeEnum::CreateStmt(stmt) => compile_create_table(stmt).map(Statement::CreateTable),
        NodeEnum::IndexStmt(stmt) => compile_create_index(stmt).map(Statement::CreateIndex),
        NodeEnum::InsertStmt(stmt) => compile_insert(stmt).map(Statement::Insert),
        NodeEnum::SelectStmt(stmt) => {
            // Standalone `VALUES (...) (...)` parses as a SelectStmt
            // with empty target_list + populated values_lists. Treat
            // it as a relation-producing statement directly.
            if stmt.target_list.is_empty() && !stmt.values_lists.is_empty() {
                let rows = compile_values_lists(&stmt.values_lists)?;
                return Ok(Statement::Values { rows });
            }
            compile_select(stmt).map(|s| Statement::Select(Box::new(s)))
        }
        NodeEnum::UpdateStmt(stmt) => compile_update(stmt).map(Statement::Update),
        NodeEnum::DeleteStmt(stmt) => compile_delete(stmt).map(Statement::Delete),
        NodeEnum::DropStmt(stmt) => compile_drop(stmt),
        NodeEnum::AlterTableStmt(stmt) => compile_alter_table(stmt).map(Statement::AlterTable),
        NodeEnum::RenameStmt(stmt) => compile_rename(stmt).map(Statement::AlterTable),
        NodeEnum::ViewStmt(stmt) => compile_create_view(stmt),
        NodeEnum::CreateSchemaStmt(stmt) => compile_create_schema(stmt),
        NodeEnum::ExplainStmt(stmt) => compile_explain(stmt),
        NodeEnum::VacuumStmt(stmt) => compile_analyze(stmt),
        NodeEnum::TruncateStmt(stmt) => compile_truncate(stmt),
        NodeEnum::TransactionStmt(stmt) => compile_transaction(stmt),
        NodeEnum::CreateSeqStmt(stmt) => {
            compile_create_sequence(stmt).map(Statement::CreateSequence)
        }
        NodeEnum::AlterSeqStmt(stmt) => compile_alter_sequence(stmt).map(Statement::AlterSequence),
        NodeEnum::CreateTableAsStmt(stmt) => compile_create_table_as(stmt),
        NodeEnum::PrepareStmt(stmt) => compile_prepare(stmt),
        NodeEnum::ExecuteStmt(stmt) => compile_execute(stmt),
        NodeEnum::DeallocateStmt(stmt) => compile_deallocate(stmt),
        NodeEnum::CreateForeignServerStmt(stmt) => {
            compile_create_foreign_server(stmt).map(Statement::CreateForeignServer)
        }
        NodeEnum::CreateForeignTableStmt(stmt) => {
            compile_create_foreign_table(stmt).map(Statement::CreateForeignTable)
        }
        NodeEnum::MergeStmt(stmt) => compile_merge(stmt).map(Statement::Merge),
        NodeEnum::CreateFunctionStmt(stmt) => {
            compile_create_function(stmt).map(|f| Statement::CreateFunction(Box::new(f)))
        }
        NodeEnum::DoStmt(stmt) => compile_do(stmt),
        NodeEnum::CallStmt(stmt) => compile_call(stmt),
        NodeEnum::VariableSetStmt(stmt) => compile_variable_set(stmt),
        NodeEnum::VariableShowStmt(stmt) => Ok(Statement::ShowVariable {
            name: stmt.name.clone(),
        }),
        NodeEnum::DiscardStmt(stmt) => Ok(Statement::Discard {
            target: discard_target(stmt.target)?,
        }),
        other => Err(SQLError::Unsupported(format!(
            "{}",
            other_node_label(other)
        ))),
    }
}

/// Map `pg_query`'s `DiscardMode` enum (1=ALL, 2=PLANS, 3=SEQUENCES,
/// 4=TEMP) to the AST's [`DiscardTarget`].
pub(super) fn other_node_label(node: &NodeEnum) -> &'static str {
    match node {
        NodeEnum::ExplainStmt(_) => "EXPLAIN",
        NodeEnum::ViewStmt(_) => "CREATE VIEW",
        NodeEnum::TransactionStmt(_) => "BEGIN/COMMIT/ROLLBACK",
        NodeEnum::PrepareStmt(_) | NodeEnum::ExecuteStmt(_) => "PREPARE/EXECUTE",
        _ => "unknown statement",
    }
}

// -------------------------------------------------------------------------
// DROP TABLE / DROP INDEX [IF EXISTS] [CASCADE]
// -------------------------------------------------------------------------

/// Lower `DROP FUNCTION` / `DROP PROCEDURE`. Each target arrives as
/// an `ObjectWithArgs`; the argument type list (when spelled) is
/// preserved as a typed signature because routine identity includes
/// `(schema, name, argument types)`.
pub fn plan_only_for_test(sql: &str) -> Result<Vec<Statement>> {
    compile(sql)
}
