//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lift a `pg_query` parse tree into the internal [`Statement`] AST.
//!
//! This is intentionally tight in scope: it covers `CREATE TABLE`,
//! `CREATE INDEX`, `INSERT`, and `SELECT` with the subset of clauses the
//! Phase 5 quickstart and parity fixture exercise. Anything outside that
//! grammar parses cleanly via `pg_query` but compiles to
//! [`SQLError::Unsupported`].

use pg_query::protobuf::Node;
use pg_query::NodeEnum;
use uqa_core::Value;

use crate::ast::{
    AlterTableAction, AlterTableStmt, BinaryOp, ColumnDef, ColumnType, CreateIndex, CreateTable,
    DeleteStmt, DropKind, DropStmt, Expr, FromClause, InsertStmt, JoinKind, OrderBy, Projection,
    SelectStmt, SetOp, SetOpKind, Statement, TransactionStmt, UpdateStmt, WindowSpec, CTE,
};
use crate::error::{Result, SQLError};

pub fn compile(sql: &str) -> Result<Vec<Statement>> {
    let parsed = pg_query::parse(sql)?;
    let mut out = Vec::with_capacity(parsed.protobuf.stmts.len());
    for raw in parsed.protobuf.stmts {
        let Some(node) = raw.stmt else { continue };
        out.push(compile_stmt(&node)?);
    }
    Ok(out)
}

fn compile_stmt(node: &Node) -> Result<Statement> {
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
                let mut rows: Vec<Vec<Expr>> = Vec::new();
                for r in &stmt.values_lists {
                    let Some(NodeEnum::List(list)) = r.node.as_ref() else {
                        continue;
                    };
                    let row: Vec<Expr> = list
                        .items
                        .iter()
                        .map(compile_expr)
                        .collect::<Result<Vec<_>>>()?;
                    rows.push(row);
                }
                return Ok(Statement::Values { rows });
            }
            compile_select(stmt).map(|s| Statement::Select(Box::new(s)))
        }
        NodeEnum::UpdateStmt(stmt) => compile_update(stmt).map(Statement::Update),
        NodeEnum::DeleteStmt(stmt) => compile_delete(stmt).map(Statement::Delete),
        NodeEnum::DropStmt(stmt) => compile_drop(stmt).map(Statement::Drop),
        NodeEnum::AlterTableStmt(stmt) => compile_alter_table(stmt).map(Statement::AlterTable),
        NodeEnum::RenameStmt(stmt) => compile_rename(stmt).map(Statement::AlterTable),
        NodeEnum::ViewStmt(stmt) => compile_create_view(stmt),
        NodeEnum::CreateSchemaStmt(stmt) => compile_create_schema(stmt),
        NodeEnum::ExplainStmt(stmt) => compile_explain(stmt),
        NodeEnum::VacuumStmt(_) => {
            // Treat ANALYZE / VACUUM ANALYZE the same way: ask the
            // engine to refresh stats. The Python reference parses
            // ANALYZE through the same node.
            Ok(Statement::Analyze { table: None })
        }
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
        NodeEnum::VariableSetStmt(stmt) => compile_variable_set(stmt),
        NodeEnum::VariableShowStmt(stmt) => Ok(Statement::ShowVariable {
            name: stmt.name.clone(),
        }),
        NodeEnum::DiscardStmt(stmt) => Ok(Statement::Discard {
            target: discard_target(stmt.target),
        }),
        other => Err(SQLError::Unsupported(format!(
            "{}",
            other_node_label(other)
        ))),
    }
}

/// Map `pg_query`'s `DiscardMode` enum (1=ALL, 2=PLANS, 3=SEQUENCES,
/// 4=TEMP) to the AST's [`DiscardTarget`].
fn discard_target(mode: i32) -> crate::ast::DiscardTarget {
    use crate::ast::DiscardTarget;
    match mode {
        2 => DiscardTarget::Plans,
        3 => DiscardTarget::Sequences,
        4 => DiscardTarget::Temp,
        _ => DiscardTarget::All,
    }
}

fn compile_variable_set(stmt: &pg_query::protobuf::VariableSetStmt) -> Result<Statement> {
    // Capture each argument as a string and join with commas. PG's
    // SET search_path TO a, b, c arrives as a list of A_Const nodes.
    let mut parts: Vec<String> = Vec::new();
    for arg in &stmt.args {
        if let Some(node) = arg.node.as_ref() {
            match node {
                NodeEnum::AConst(c) => match c.val.as_ref() {
                    Some(pg_query::protobuf::a_const::Val::Sval(sval)) => {
                        parts.push(sval.sval.clone());
                    }
                    Some(pg_query::protobuf::a_const::Val::Ival(iv)) => {
                        parts.push(iv.ival.to_string());
                    }
                    _ => {}
                },
                NodeEnum::TypeCast(tc) => {
                    if let Some(NodeEnum::AConst(c)) = tc.arg.as_ref().and_then(|a| a.node.as_ref())
                    {
                        if let Some(pg_query::protobuf::a_const::Val::Sval(sval)) = c.val.as_ref() {
                            parts.push(sval.sval.clone());
                        }
                    }
                }
                NodeEnum::String(s) => parts.push(s.sval.clone()),
                _ => {}
            }
        }
    }
    Ok(Statement::SetVariable {
        name: stmt.name.clone(),
        value: parts.join(","),
    })
}

fn compile_create_sequence(
    stmt: &pg_query::protobuf::CreateSeqStmt,
) -> Result<crate::ast::CreateSequence> {
    use crate::ast::CreateSequence;
    let name = stmt
        .sequence
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SQLError::Internal("CREATE SEQUENCE without name".into()))?;
    let mut start = 1_i64;
    let mut increment = 1_i64;
    for opt in &stmt.options {
        if let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() {
            let key = elem.defname.to_ascii_lowercase();
            let v = match elem.arg.as_ref().and_then(|a| a.node.as_ref()) {
                Some(NodeEnum::Integer(i)) => i64::from(i.ival),
                Some(NodeEnum::Float(f)) => f.fval.parse::<f64>().unwrap_or(0.0) as i64,
                Some(NodeEnum::String(s)) => s.sval.parse::<i64>().unwrap_or(0),
                _ => continue,
            };
            match key.as_str() {
                "start" => start = v,
                "increment" => increment = v,
                _ => {}
            }
        }
    }
    Ok(CreateSequence {
        name,
        if_not_exists: stmt.if_not_exists,
        start,
        increment,
    })
}

fn compile_alter_sequence(
    stmt: &pg_query::protobuf::AlterSeqStmt,
) -> Result<crate::ast::AlterSequence> {
    use crate::ast::AlterSequence;
    let name = stmt
        .sequence
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SQLError::Internal("ALTER SEQUENCE without name".into()))?;
    let mut alter = AlterSequence {
        name,
        ..Default::default()
    };
    for opt in &stmt.options {
        if let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() {
            let key = elem.defname.to_ascii_lowercase();
            let v_opt: Option<i64> = match elem.arg.as_ref().and_then(|a| a.node.as_ref()) {
                Some(NodeEnum::Integer(i)) => Some(i64::from(i.ival)),
                Some(NodeEnum::Float(f)) => Some(f.fval.parse::<f64>().unwrap_or(0.0) as i64),
                Some(NodeEnum::String(s)) => s.sval.parse::<i64>().ok(),
                _ => None,
            };
            match key.as_str() {
                "restart" => alter.restart = Some(v_opt),
                "increment" => alter.increment = v_opt,
                "start" => alter.start = v_opt,
                _ => {}
            }
        }
    }
    Ok(alter)
}

fn compile_create_table_as(stmt: &pg_query::protobuf::CreateTableAsStmt) -> Result<Statement> {
    let into = stmt
        .into
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE TABLE AS without target".into()))?;
    let name = into
        .rel
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SQLError::Internal("CREATE TABLE AS target has no name".into()))?;
    let body = stmt
        .query
        .as_deref()
        .ok_or_else(|| SQLError::Internal("CREATE TABLE AS without body".into()))?;
    let inner = body
        .node
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE TABLE AS body empty".into()))?;
    let select = match inner {
        NodeEnum::SelectStmt(s) => compile_select(s)?,
        other => {
            return Err(SQLError::Unsupported(format!(
                "CREATE TABLE AS body must be SELECT, got {other:?}"
            )));
        }
    };
    Ok(Statement::CreateTableAs {
        name,
        if_not_exists: stmt.if_not_exists,
        body: Box::new(select),
    })
}

fn compile_prepare(stmt: &pg_query::protobuf::PrepareStmt) -> Result<Statement> {
    let name = stmt.name.clone();
    let body = stmt
        .query
        .as_deref()
        .ok_or_else(|| SQLError::Internal("PREPARE without body".into()))?;
    let inner = compile_stmt(body)?;
    Ok(Statement::Prepare {
        name,
        body: Box::new(inner),
    })
}

fn compile_execute(stmt: &pg_query::protobuf::ExecuteStmt) -> Result<Statement> {
    let name = stmt.name.clone();
    let mut params: Vec<Expr> = Vec::with_capacity(stmt.params.len());
    for p in &stmt.params {
        params.push(compile_expr(p)?);
    }
    Ok(Statement::Execute { name, params })
}

fn compile_deallocate(stmt: &pg_query::protobuf::DeallocateStmt) -> Result<Statement> {
    let name = if stmt.name.is_empty() {
        None
    } else {
        Some(stmt.name.clone())
    };
    Ok(Statement::Deallocate { name })
}

fn compile_create_foreign_server(
    stmt: &pg_query::protobuf::CreateForeignServerStmt,
) -> Result<crate::ast::CreateForeignServer> {
    use crate::ast::CreateForeignServer;
    Ok(CreateForeignServer {
        name: stmt.servername.clone(),
        fdw_type: stmt.fdwname.clone(),
        options: collect_def_elem_options(&stmt.options),
        if_not_exists: stmt.if_not_exists,
    })
}

fn compile_create_foreign_table(
    stmt: &pg_query::protobuf::CreateForeignTableStmt,
) -> Result<crate::ast::CreateForeignTable> {
    use crate::ast::CreateForeignTable;
    let base = stmt
        .base_stmt
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE FOREIGN TABLE without base".into()))?;
    let name = base
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SQLError::Internal("CREATE FOREIGN TABLE without name".into()))?;
    let mut columns: Vec<ColumnDef> = Vec::new();
    for elt in &base.table_elts {
        if let Some(NodeEnum::ColumnDef(col)) = elt.node.as_ref() {
            columns.push(compile_column_def(col)?);
        }
    }
    Ok(CreateForeignTable {
        name,
        server_name: stmt.servername.clone(),
        columns,
        options: collect_def_elem_options(&stmt.options),
        if_not_exists: base.if_not_exists,
    })
}

fn compile_merge(stmt: &pg_query::protobuf::MergeStmt) -> Result<crate::ast::MergeStmt> {
    use crate::ast::{MergeStmt, MergeWhen};
    use pg_query::protobuf::{CmdType, MergeMatchKind};
    let target = stmt
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SQLError::Internal("MERGE without target".into()))?;
    let target_alias = stmt
        .relation
        .as_ref()
        .and_then(|r| r.alias.as_ref())
        .map(|a| a.aliasname.clone())
        .filter(|s| !s.is_empty());
    let source_node = stmt
        .source_relation
        .as_deref()
        .ok_or_else(|| SQLError::Internal("MERGE without USING".into()))?;
    let source = compile_from_node(source_node)?;
    let join_condition_node = stmt
        .join_condition
        .as_deref()
        .ok_or_else(|| SQLError::Internal("MERGE without ON".into()))?;
    let join_condition = compile_expr(join_condition_node)?;

    let mut when_clauses: Vec<MergeWhen> = Vec::with_capacity(stmt.merge_when_clauses.len());
    for clause in &stmt.merge_when_clauses {
        let Some(NodeEnum::MergeWhenClause(w)) = clause.node.as_ref() else {
            continue;
        };
        let condition = w
            .condition
            .as_deref()
            .map(|c| compile_expr(c))
            .transpose()?;
        let matched = matches!(w.match_kind(), MergeMatchKind::MergeWhenMatched);
        let cmd = w.command_type();
        match cmd {
            CmdType::CmdUpdate => {
                let mut assignments: Vec<(String, Expr)> = Vec::new();
                for tgt in &w.target_list {
                    let Some(NodeEnum::ResTarget(rt)) = tgt.node.as_ref() else {
                        continue;
                    };
                    let val = rt
                        .val
                        .as_ref()
                        .ok_or_else(|| SQLError::Internal("MERGE UPDATE without value".into()))?;
                    assignments.push((rt.name.clone(), compile_expr(val)?));
                }
                when_clauses.push(MergeWhen::UpdateMatched {
                    condition,
                    assignments,
                });
                let _ = matched; // Update only legal after MATCHED.
            }
            CmdType::CmdDelete => {
                when_clauses.push(MergeWhen::DeleteMatched { condition });
            }
            CmdType::CmdInsert => {
                let mut columns: Vec<String> = Vec::with_capacity(w.target_list.len());
                for tgt in &w.target_list {
                    if let Some(NodeEnum::ResTarget(rt)) = tgt.node.as_ref() {
                        columns.push(rt.name.clone());
                    }
                }
                let values: Vec<Expr> = w
                    .values
                    .iter()
                    .map(compile_expr)
                    .collect::<Result<Vec<_>>>()?;
                when_clauses.push(MergeWhen::InsertNotMatched {
                    condition,
                    columns,
                    values,
                });
            }
            CmdType::CmdNothing => {
                if matched {
                    when_clauses.push(MergeWhen::NothingMatched { condition });
                } else {
                    when_clauses.push(MergeWhen::NothingNotMatched { condition });
                }
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "MERGE WHEN command {other:?}"
                )));
            }
        }
    }

    Ok(MergeStmt {
        target,
        target_alias,
        source,
        join_condition,
        when_clauses,
    })
}

fn collect_def_elem_options(nodes: &[Node]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for opt in nodes {
        if let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() {
            let value = match elem.arg.as_ref().and_then(|a| a.node.as_ref()) {
                Some(NodeEnum::String(s)) => s.sval.clone(),
                Some(NodeEnum::Integer(i)) => i.ival.to_string(),
                Some(NodeEnum::Float(f)) => f.fval.clone(),
                _ => String::new(),
            };
            out.push((elem.defname.clone(), value));
        }
    }
    out
}

fn compile_create_view(stmt: &pg_query::protobuf::ViewStmt) -> Result<Statement> {
    let name = stmt
        .view
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SQLError::Internal("CREATE VIEW without name".into()))?;
    let body = stmt
        .query
        .as_deref()
        .ok_or_else(|| SQLError::Internal("CREATE VIEW without body".into()))?;
    let inner = body
        .node
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE VIEW body empty".into()))?;
    let select = match inner {
        NodeEnum::SelectStmt(s) => compile_select(s)?,
        other => {
            return Err(SQLError::Unsupported(format!(
                "CREATE VIEW body must be SELECT, got {other:?}"
            )));
        }
    };
    Ok(Statement::CreateView {
        name,
        body: Box::new(select),
        or_replace: stmt.replace,
    })
}

fn compile_create_schema(stmt: &pg_query::protobuf::CreateSchemaStmt) -> Result<Statement> {
    let name = if stmt.schemaname.is_empty() {
        return Err(SQLError::Internal("CREATE SCHEMA without name".into()));
    } else {
        stmt.schemaname.clone()
    };
    Ok(Statement::CreateSchema {
        name,
        if_not_exists: stmt.if_not_exists,
    })
}

fn compile_explain(stmt: &pg_query::protobuf::ExplainStmt) -> Result<Statement> {
    let body = stmt
        .query
        .as_deref()
        .ok_or_else(|| SQLError::Internal("EXPLAIN without body".into()))?;
    let mut analyze = false;
    let mut verbose = false;
    let mut format: Option<String> = None;
    for opt in &stmt.options {
        if let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() {
            let name = elem.defname.to_ascii_lowercase();
            match name.as_str() {
                "analyze" => analyze = true,
                "verbose" => verbose = true,
                "format" => {
                    if let Some(NodeEnum::String(s)) =
                        elem.arg.as_ref().and_then(|a| a.node.as_ref())
                    {
                        format = Some(s.sval.clone());
                    }
                }
                _ => {}
            }
        }
    }
    let inner = compile_stmt(body)?;
    Ok(Statement::Explain {
        analyze,
        verbose,
        format,
        body: Box::new(inner),
    })
}

fn compile_truncate(stmt: &pg_query::protobuf::TruncateStmt) -> Result<Statement> {
    let mut tables = Vec::new();
    for r in &stmt.relations {
        if let Some(NodeEnum::RangeVar(rv)) = r.node.as_ref() {
            tables.push(rv.relname.clone());
        }
    }
    let cascade = matches!(
        stmt.behavior(),
        pg_query::protobuf::DropBehavior::DropCascade
    );
    Ok(Statement::Truncate { tables, cascade })
}

fn compile_transaction(stmt: &pg_query::protobuf::TransactionStmt) -> Result<Statement> {
    use pg_query::protobuf::TransactionStmtKind;
    let kind = match stmt.kind() {
        TransactionStmtKind::TransStmtBegin | TransactionStmtKind::TransStmtStart => {
            TransactionStmt::Begin
        }
        TransactionStmtKind::TransStmtCommit => TransactionStmt::Commit,
        TransactionStmtKind::TransStmtRollback => TransactionStmt::Rollback,
        TransactionStmtKind::TransStmtSavepoint => {
            TransactionStmt::Savepoint(stmt.savepoint_name.clone())
        }
        TransactionStmtKind::TransStmtRelease => {
            TransactionStmt::ReleaseSavepoint(stmt.savepoint_name.clone())
        }
        TransactionStmtKind::TransStmtRollbackTo => {
            TransactionStmt::RollbackToSavepoint(stmt.savepoint_name.clone())
        }
        other => {
            return Err(SQLError::Unsupported(format!("transaction kind {other:?}")));
        }
    };
    Ok(Statement::Transaction(kind))
}

fn compile_update(stmt: &pg_query::protobuf::UpdateStmt) -> Result<UpdateStmt> {
    let table = stmt
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SQLError::Internal("UPDATE without relation".into()))?;
    let mut assignments = Vec::new();
    for target_node in &stmt.target_list {
        let Some(inner) = target_node.node.as_ref() else {
            continue;
        };
        if let NodeEnum::ResTarget(rt) = inner {
            let value = rt
                .val
                .as_ref()
                .ok_or_else(|| SQLError::Internal("UPDATE assignment without value".into()))?;
            assignments.push((rt.name.clone(), compile_expr(value)?));
        }
    }
    let r#where = stmt
        .where_clause
        .as_ref()
        .map(|w| compile_expr(w))
        .transpose()?;
    let from = match stmt.from_clause.first() {
        Some(node) => Some(compile_from_node(node)?),
        None => None,
    };
    Ok(UpdateStmt {
        table,
        assignments,
        r#where,
        from,
    })
}

fn compile_delete(stmt: &pg_query::protobuf::DeleteStmt) -> Result<DeleteStmt> {
    let table = stmt
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SQLError::Internal("DELETE without relation".into()))?;
    let r#where = stmt
        .where_clause
        .as_ref()
        .map(|w| compile_expr(w))
        .transpose()?;
    let using = match stmt.using_clause.first() {
        Some(node) => Some(compile_from_node(node)?),
        None => None,
    };
    Ok(DeleteStmt {
        table,
        r#where,
        using,
    })
}

fn other_node_label(node: &NodeEnum) -> &'static str {
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

fn compile_drop(stmt: &pg_query::protobuf::DropStmt) -> Result<DropStmt> {
    use pg_query::protobuf::{DropBehavior, ObjectType};
    let kind = match stmt.remove_type() {
        ObjectType::ObjectTable => DropKind::Table,
        ObjectType::ObjectIndex => DropKind::Index,
        ObjectType::ObjectView => DropKind::View,
        ObjectType::ObjectSchema => DropKind::Schema,
        other => {
            return Err(SQLError::Unsupported(format!(
                "DROP target {other:?} not supported"
            )));
        }
    };
    let mut names = Vec::new();
    for object in &stmt.objects {
        let Some(inner) = object.node.as_ref() else {
            continue;
        };
        match inner {
            NodeEnum::List(list) => {
                let parts: Vec<String> = list
                    .items
                    .iter()
                    .filter_map(|n| extract_string(n).ok())
                    .collect();
                if let Some(last) = parts.last() {
                    names.push(last.clone());
                }
            }
            NodeEnum::String(s) => names.push(s.sval.clone()),
            other => {
                return Err(SQLError::Unsupported(format!(
                    "DROP object node {other:?} not supported"
                )));
            }
        }
    }
    if names.is_empty() {
        return Err(SQLError::Internal("DROP without target name".into()));
    }
    let cascade = matches!(stmt.behavior(), DropBehavior::DropCascade);
    Ok(DropStmt {
        kind,
        names,
        if_exists: stmt.missing_ok,
        cascade,
    })
}

// -------------------------------------------------------------------------
// ALTER TABLE { ADD COLUMN | DROP COLUMN | RENAME COLUMN | RENAME TO }
// -------------------------------------------------------------------------

fn compile_alter_table(stmt: &pg_query::protobuf::AlterTableStmt) -> Result<AlterTableStmt> {
    use pg_query::protobuf::{AlterTableType, DropBehavior};
    let table = stmt
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SQLError::Internal("ALTER TABLE without relation".into()))?;
    let if_exists = stmt.missing_ok;
    let cmd = stmt
        .cmds
        .first()
        .ok_or_else(|| SQLError::Internal("ALTER TABLE without command".into()))?;
    let inner = cmd
        .node
        .as_ref()
        .ok_or_else(|| SQLError::Internal("ALTER TABLE command body empty".into()))?;
    let cmd = match inner {
        NodeEnum::AlterTableCmd(c) => c,
        other => {
            return Err(SQLError::Unsupported(format!(
                "ALTER TABLE command {other:?}"
            )));
        }
    };
    let action = match cmd.subtype() {
        AlterTableType::AtAddColumn => {
            let def_inner = cmd
                .def
                .as_ref()
                .and_then(|d| d.node.as_ref())
                .ok_or_else(|| SQLError::Internal("ADD COLUMN without ColumnDef".into()))?;
            let col_def = match def_inner {
                NodeEnum::ColumnDef(c) => compile_column_def(c)?,
                other => {
                    return Err(SQLError::Internal(format!(
                        "ADD COLUMN expected ColumnDef, got {other:?}"
                    )));
                }
            };
            AlterTableAction::AddColumn {
                column: col_def,
                if_not_exists: cmd.missing_ok,
            }
        }
        AlterTableType::AtDropColumn => AlterTableAction::DropColumn {
            name: cmd.name.clone(),
            if_exists: cmd.missing_ok,
            cascade: matches!(cmd.behavior(), DropBehavior::DropCascade),
        },
        AlterTableType::AtAlterColumnType
        | AlterTableType::AtSetNotNull
        | AlterTableType::AtDropNotNull
        | AlterTableType::AtColumnDefault => {
            return Err(SQLError::Unsupported(format!(
                "ALTER COLUMN action {:?}",
                cmd.subtype()
            )));
        }
        other => {
            return Err(SQLError::Unsupported(format!(
                "ALTER TABLE action {other:?}"
            )));
        }
    };
    Ok(AlterTableStmt {
        table,
        if_exists,
        action,
    })
}

fn compile_rename(stmt: &pg_query::protobuf::RenameStmt) -> Result<AlterTableStmt> {
    use pg_query::protobuf::ObjectType;
    let table = stmt
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SQLError::Internal("RENAME without relation".into()))?;
    let action = match stmt.rename_type() {
        ObjectType::ObjectColumn => AlterTableAction::RenameColumn {
            from: stmt.subname.clone(),
            to: stmt.newname.clone(),
        },
        ObjectType::ObjectTable => AlterTableAction::RenameTable {
            to: stmt.newname.clone(),
        },
        other => {
            return Err(SQLError::Unsupported(format!(
                "RENAME target {other:?} not supported"
            )));
        }
    };
    Ok(AlterTableStmt {
        table,
        if_exists: stmt.missing_ok,
        action,
    })
}

fn extract_string(node: &Node) -> Result<String> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("missing string node".into()));
    };
    match inner {
        NodeEnum::String(s) => Ok(s.sval.clone()),
        _ => Err(SQLError::Internal(format!(
            "expected String node, got {inner:?}"
        ))),
    }
}

/// Translate a `#>` / `#>>` operator into the argument list of
/// `json_extract_path`. The right-hand side is a Postgres text-array
/// literal like `'{a,b,c}'`; we split it into individual literal
/// segments so the scalar function can walk the path.
fn json_path_args(lhs: Expr, rhs: Expr) -> Vec<Expr> {
    let segments = match &rhs {
        Expr::Literal(uqa_core::Value::Str(s)) => s
            .trim_matches(|c: char| c == '{' || c == '}')
            .split(',')
            .map(|seg| Expr::Literal(uqa_core::Value::Str(seg.trim().to_string())))
            .collect::<Vec<_>>(),
        Expr::Literal(uqa_core::Value::List(items)) => items
            .iter()
            .map(|v| Expr::Literal(v.clone()))
            .collect::<Vec<_>>(),
        _ => vec![rhs],
    };
    let mut out = Vec::with_capacity(segments.len() + 1);
    out.push(lhs);
    out.extend(segments);
    out
}

// -------------------------------------------------------------------------
// CREATE TABLE
// -------------------------------------------------------------------------

fn compile_create_table(stmt: &pg_query::protobuf::CreateStmt) -> Result<CreateTable> {
    use crate::ast::{ForeignKey, TableCheck};
    let name = stmt
        .relation
        .as_ref()
        .map(|r| {
            if r.schemaname.is_empty() {
                r.relname.clone()
            } else {
                format!("{}.{}", r.schemaname, r.relname)
            }
        })
        .unwrap_or_default();
    if name.is_empty() {
        return Err(SQLError::Internal("CREATE TABLE without name".into()));
    }
    let mut columns = Vec::new();
    let mut checks: Vec<TableCheck> = Vec::new();
    let mut foreign_keys: Vec<ForeignKey> = Vec::new();
    for elt in &stmt.table_elts {
        let Some(inner) = elt.node.as_ref() else {
            continue;
        };
        match inner {
            NodeEnum::ColumnDef(col) => {
                columns.push(compile_column_def(col)?);
            }
            NodeEnum::Constraint(cstr) => match cstr.contype() {
                pg_query::protobuf::ConstrType::ConstrCheck => {
                    if let Some(raw) = cstr.raw_expr.as_deref() {
                        let expr = compile_expr(raw)?;
                        let cname = if cstr.conname.is_empty() {
                            None
                        } else {
                            Some(cstr.conname.clone())
                        };
                        checks.push(TableCheck { name: cname, expr });
                    }
                }
                pg_query::protobuf::ConstrType::ConstrForeign => {
                    let local_columns: Vec<String> = cstr
                        .fk_attrs
                        .iter()
                        .filter_map(|n| extract_string(n).ok())
                        .collect();
                    let ref_table = cstr
                        .pktable
                        .as_ref()
                        .map(|r| r.relname.clone())
                        .unwrap_or_default();
                    let ref_columns: Vec<String> = cstr
                        .pk_attrs
                        .iter()
                        .filter_map(|n| extract_string(n).ok())
                        .collect();
                    if !local_columns.is_empty() && !ref_table.is_empty() && !ref_columns.is_empty()
                    {
                        let cname = if cstr.conname.is_empty() {
                            None
                        } else {
                            Some(cstr.conname.clone())
                        };
                        foreign_keys.push(ForeignKey {
                            name: cname,
                            local_columns,
                            ref_table,
                            ref_columns,
                        });
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(CreateTable {
        name,
        columns,
        if_not_exists: stmt.if_not_exists,
        checks,
        foreign_keys,
    })
}

fn compile_column_def(col: &pg_query::protobuf::ColumnDef) -> Result<ColumnDef> {
    let name = col.colname.clone();
    let raw_type = raw_type_name(col).unwrap_or_default();
    let ty = compile_type_name(col)?;
    let auto_increment = matches!(raw_type.as_str(), "serial" | "bigserial");
    let mut primary_key = false;
    let mut not_null = false;
    let mut unique = false;
    let mut default: Option<Expr> = None;
    let mut check: Option<Expr> = None;
    let mut references: Option<crate::ast::ForeignKeyRef> = None;
    for c in &col.constraints {
        let Some(inner) = c.node.as_ref() else {
            continue;
        };
        if let NodeEnum::Constraint(cstr) = inner {
            match cstr.contype() {
                pg_query::protobuf::ConstrType::ConstrPrimary => primary_key = true,
                pg_query::protobuf::ConstrType::ConstrNotnull => not_null = true,
                pg_query::protobuf::ConstrType::ConstrUnique => unique = true,
                pg_query::protobuf::ConstrType::ConstrDefault => {
                    if let Some(raw) = cstr.raw_expr.as_deref() {
                        default = Some(compile_expr(raw)?);
                    }
                }
                pg_query::protobuf::ConstrType::ConstrCheck => {
                    if let Some(raw) = cstr.raw_expr.as_deref() {
                        check = Some(compile_expr(raw)?);
                    }
                }
                pg_query::protobuf::ConstrType::ConstrForeign => {
                    let table = cstr
                        .pktable
                        .as_ref()
                        .map(|r| r.relname.clone())
                        .unwrap_or_default();
                    let column = cstr
                        .pk_attrs
                        .iter()
                        .find_map(|n| extract_string(n).ok())
                        .unwrap_or_default();
                    if !table.is_empty() && !column.is_empty() {
                        references = Some(crate::ast::ForeignKeyRef { table, column });
                    }
                }
                _ => {}
            }
        }
    }
    // Postgres treats `SERIAL` / `BIGSERIAL` as `NOT NULL` by definition.
    if auto_increment {
        not_null = true;
    }
    Ok(ColumnDef {
        name,
        ty,
        primary_key,
        not_null,
        auto_increment,
        unique,
        default,
        check,
        references,
    })
}

fn raw_type_name(col: &pg_query::protobuf::ColumnDef) -> Option<String> {
    let type_name = col.type_name.as_ref()?;
    let names: Vec<String> = type_name
        .names
        .iter()
        .filter_map(|n| extract_string(n).ok())
        .collect();
    Some(names.last().cloned().unwrap_or_default().to_lowercase())
}

fn compile_type_name(col: &pg_query::protobuf::ColumnDef) -> Result<ColumnType> {
    let Some(type_name) = col.type_name.as_ref() else {
        return Err(SQLError::Internal(format!(
            "column `{}` has no type",
            col.colname
        )));
    };
    let names: Vec<String> = type_name
        .names
        .iter()
        .filter_map(|n| extract_string(n).ok())
        .collect();
    let raw = names.last().cloned().unwrap_or_default().to_lowercase();
    match raw.as_str() {
        "int" | "int4" | "integer" | "bigint" | "int8" | "smallint" | "int2" | "serial"
        | "bigserial" | "serial4" | "serial8" => Ok(ColumnType::Integer),
        "text" | "varchar" | "character" | "char" | "bpchar" | "name" | "uuid" => {
            Ok(ColumnType::Text)
        }
        "bool" | "boolean" => Ok(ColumnType::Integer),
        "real" | "float4" | "float8" | "double" | "double precision" => {
            Ok(ColumnType::Real)
        }
        "numeric" | "decimal" => {
            let mut typmods_iter = type_name.typmods.iter();
            let precision = typmods_iter
                .next()
                .map(|n| expect_integer_const(n).map(|v| v as u32))
                .transpose()?;
            let scale = typmods_iter
                .next()
                .map(|n| expect_integer_const(n).map(|v| v as u32))
                .transpose()?;
            // PostgreSQL semantics: NUMERIC(precision) without an
            // explicit scale defaults to scale=0, rounding to integers.
            let scale = scale.or(precision.map(|_| 0));
            Ok(ColumnType::Numeric { precision, scale })
        }
        "date"
        | "time"
        | "timetz"
        | "timestamp"
        | "timestamptz"
        | "timestamp without time zone"
        | "timestamp with time zone"
        | "time without time zone"
        | "time with time zone" => Ok(ColumnType::Text),
        "json" | "jsonb" => Ok(ColumnType::Text),
        "vector" => {
            // VECTOR(N): the dimension is the only typmod argument.
            let Some(arg) = type_name.typmods.first() else {
                return Err(SQLError::Unsupported(
                    "VECTOR without dimension is not supported".into(),
                ));
            };
            let dim = expect_integer_const(arg)? as u32;
            Ok(ColumnType::Vector(dim))
        }
        other => Err(SQLError::Unsupported(format!(
            "column type `{other}` is not supported"
        ))),
    }
}

fn expect_integer_const(node: &Node) -> Result<i64> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("missing const node".into()));
    };
    match inner {
        NodeEnum::AConst(c) => match &c.val {
            Some(pg_query::protobuf::a_const::Val::Ival(i)) => Ok(i64::from(i.ival)),
            Some(pg_query::protobuf::a_const::Val::Fval(f)) => f
                .fval
                .parse::<f64>()
                .map(|v| v as i64)
                .map_err(|e| SQLError::Internal(e.to_string())),
            other => Err(SQLError::Internal(format!(
                "expected integer constant, got {other:?}"
            ))),
        },
        _ => Err(SQLError::Internal(format!(
            "expected A_Const, got {inner:?}"
        ))),
    }
}

// -------------------------------------------------------------------------
// CREATE INDEX
// -------------------------------------------------------------------------

fn compile_create_index(stmt: &pg_query::protobuf::IndexStmt) -> Result<CreateIndex> {
    let table = stmt
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .unwrap_or_default();
    let access_method = stmt.access_method.clone();
    let mut columns = Vec::new();
    for elt in &stmt.index_params {
        let Some(inner) = elt.node.as_ref() else {
            continue;
        };
        if let NodeEnum::IndexElem(idx) = inner {
            if !idx.name.is_empty() {
                columns.push(idx.name.clone());
            }
        }
    }
    let name = if stmt.idxname.is_empty() {
        None
    } else {
        Some(stmt.idxname.clone())
    };
    let mut options = Vec::new();
    for opt in &stmt.options {
        let Some(inner) = opt.node.as_ref() else {
            continue;
        };
        if let NodeEnum::DefElem(elem) = inner {
            let key = elem.defname.clone();
            let value = elem
                .arg
                .as_ref()
                .and_then(|n| n.node.as_ref())
                .map(|inner| match inner {
                    NodeEnum::String(s) => s.sval.clone(),
                    NodeEnum::Integer(i) => i.ival.to_string(),
                    NodeEnum::Float(f) => f.fval.clone(),
                    NodeEnum::TypeName(t) => t
                        .names
                        .iter()
                        .filter_map(|n| extract_string(n).ok())
                        .collect::<Vec<_>>()
                        .join("."),
                    other => format!("{other:?}"),
                })
                .unwrap_or_default();
            options.push((key, value));
        }
    }
    Ok(CreateIndex {
        name,
        table,
        access_method,
        columns,
        if_not_exists: stmt.if_not_exists,
        options,
    })
}

// -------------------------------------------------------------------------
// INSERT
// -------------------------------------------------------------------------

fn compile_insert(stmt: &pg_query::protobuf::InsertStmt) -> Result<InsertStmt> {
    let table = stmt
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SQLError::Internal("INSERT without relation".into()))?;
    let columns: Vec<String> = stmt
        .cols
        .iter()
        .filter_map(|c| {
            c.node.as_ref().and_then(|inner| match inner {
                NodeEnum::ResTarget(r) => Some(r.name.clone()),
                _ => None,
            })
        })
        .collect();
    let select_node = stmt
        .select_stmt
        .as_ref()
        .ok_or_else(|| SQLError::Unsupported("INSERT without VALUES".into()))?;
    let select_inner = select_node
        .node
        .as_ref()
        .ok_or_else(|| SQLError::Internal("INSERT select_stmt empty".into()))?;
    let select = match select_inner {
        NodeEnum::SelectStmt(s) => s,
        _ => return Err(SQLError::Unsupported("INSERT body must be SELECT".into())),
    };
    let mut rows = Vec::new();
    for row_node in &select.values_lists {
        let Some(inner) = row_node.node.as_ref() else {
            continue;
        };
        let list = match inner {
            NodeEnum::List(l) => l,
            _ => continue,
        };
        let row: Vec<Expr> = list
            .items
            .iter()
            .map(compile_expr)
            .collect::<Result<Vec<_>>>()?;
        rows.push(row);
    }
    // INSERT ... SELECT: when the body has no values_lists but does
    // have a from_clause / target_list, treat it as `INSERT FROM
    // SELECT` and forward the inner SELECT.
    let select_source =
        if rows.is_empty() && (!select.from_clause.is_empty() || !select.target_list.is_empty()) {
            Some(Box::new(compile_select(select)?))
        } else {
            None
        };
    let on_conflict = stmt
        .on_conflict_clause
        .as_ref()
        .map(|c| compile_on_conflict(c.as_ref()))
        .transpose()?;
    Ok(InsertStmt {
        table,
        columns,
        rows,
        select_source,
        on_conflict,
    })
}

fn compile_on_conflict(
    clause: &pg_query::protobuf::OnConflictClause,
) -> Result<crate::ast::OnConflict> {
    use crate::ast::{OnConflict, OnConflictAction};
    use pg_query::protobuf::OnConflictAction as PgAction;

    let conflict_columns: Vec<String> = clause
        .infer
        .as_ref()
        .map(|infer| {
            infer
                .index_elems
                .iter()
                .filter_map(|elem| {
                    elem.node.as_ref().and_then(|inner| match inner {
                        NodeEnum::IndexElem(ie) => {
                            if ie.name.is_empty() {
                                None
                            } else {
                                Some(ie.name.clone())
                            }
                        }
                        _ => None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let action = match clause.action() {
        PgAction::OnconflictNothing => OnConflictAction::Nothing,
        PgAction::OnconflictUpdate => {
            let mut assignments: Vec<(String, Expr)> = Vec::new();
            for tgt in &clause.target_list {
                let Some(inner) = tgt.node.as_ref() else {
                    continue;
                };
                let NodeEnum::ResTarget(rt) = inner else {
                    continue;
                };
                let Some(val) = rt.val.as_ref() else { continue };
                let expr = compile_expr(val)?;
                assignments.push((rt.name.clone(), expr));
            }
            let where_clause = clause
                .where_clause
                .as_ref()
                .map(|w| compile_expr(w))
                .transpose()?;
            OnConflictAction::Update {
                assignments,
                r#where: where_clause,
            }
        }
        PgAction::OnconflictNone | PgAction::Undefined => {
            return Err(SQLError::Unsupported(
                "ON CONFLICT without action specifier".into(),
            ));
        }
    };

    Ok(OnConflict {
        conflict_columns,
        action,
    })
}

// -------------------------------------------------------------------------
// SELECT
// -------------------------------------------------------------------------

fn compile_from_node(node: &Node) -> Result<FromClause> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("empty FROM node".into()));
    };
    match inner {
        NodeEnum::RangeVar(r) => Ok(FromClause::Table {
            name: if r.schemaname.is_empty() {
                r.relname.clone()
            } else {
                format!("{}.{}", r.schemaname, r.relname)
            },
            alias: r.alias.as_ref().and_then(|a| {
                if a.aliasname.is_empty() {
                    None
                } else {
                    Some(a.aliasname.clone())
                }
            }),
        }),
        NodeEnum::JoinExpr(j) => {
            let left = j
                .larg
                .as_ref()
                .ok_or_else(|| SQLError::Internal("JOIN missing left".into()))?;
            let right = j
                .rarg
                .as_ref()
                .ok_or_else(|| SQLError::Internal("JOIN missing right".into()))?;
            let kind = match j.jointype() {
                pg_query::protobuf::JoinType::JoinInner => JoinKind::Inner,
                pg_query::protobuf::JoinType::JoinLeft => JoinKind::Left,
                pg_query::protobuf::JoinType::JoinRight => JoinKind::Right,
                pg_query::protobuf::JoinType::JoinFull => JoinKind::Full,
                other => {
                    return Err(SQLError::Unsupported(format!("JOIN type {other:?}")));
                }
            };
            let on = j.quals.as_deref().map(compile_expr).transpose()?;
            let lateral = right_is_lateral(right);
            Ok(FromClause::Join {
                left: Box::new(compile_from_node(left)?),
                right: Box::new(compile_from_node(right)?),
                kind,
                on,
                lateral,
            })
        }
        NodeEnum::RangeSubselect(rs) => {
            let body_node = rs
                .subquery
                .as_deref()
                .ok_or_else(|| SQLError::Internal("FROM (subquery) without body".into()))?;
            let inner = body_node
                .node
                .as_ref()
                .ok_or_else(|| SQLError::Internal("subquery body empty".into()))?;
            let select = match inner {
                NodeEnum::SelectStmt(s) => compile_select(s)?,
                other => {
                    return Err(SQLError::Unsupported(format!(
                        "FROM (subquery) body must be SELECT, got {other:?}"
                    )));
                }
            };
            // Standalone VALUES land here as a SelectStmt with empty
            // target_list and a values_lists -- promote to
            // FromClause::Values for the engine fast path.
            let (alias, column_aliases) = compile_alias(rs.alias.as_ref());
            let body_inner = body_node.node.as_ref().unwrap();
            if let NodeEnum::SelectStmt(s) = body_inner {
                if !s.values_lists.is_empty() {
                    let mut rows: Vec<Vec<Expr>> = Vec::new();
                    for r in &s.values_lists {
                        let Some(NodeEnum::List(list)) = r.node.as_ref() else {
                            continue;
                        };
                        let row: Vec<Expr> = list
                            .items
                            .iter()
                            .map(compile_expr)
                            .collect::<Result<Vec<_>>>()?;
                        rows.push(row);
                    }
                    return Ok(FromClause::Values {
                        rows,
                        alias,
                        column_aliases,
                    });
                }
            }
            Ok(FromClause::Subquery {
                body: Box::new(select),
                alias,
                column_aliases,
            })
        }
        NodeEnum::RangeFunction(rf) => {
            // The first function in `functions` carries the call. Take
            // that node verbatim and re-use compile_expr to lift it
            // into an Expr::Func, then peel back the name + args.
            let Some(first_node) = rf.functions.first() else {
                return Err(SQLError::Internal("RangeFunction without functions".into()));
            };
            // RangeFunction.functions is a list of `[FuncCall, alias_definition]`
            // pairs encoded as a List. Take the first element of the
            // first pair as the call.
            let call = match first_node.node.as_ref() {
                Some(NodeEnum::List(l)) => l
                    .items
                    .first()
                    .ok_or_else(|| SQLError::Internal("RangeFunction empty pair".into()))?,
                _ => first_node,
            };
            let expr = compile_expr(call)?;
            let (name, args) = match expr {
                Expr::Func { name, args, .. } => (name, args),
                other => {
                    return Err(SQLError::Unsupported(format!(
                        "RangeFunction body must be a function call, got {other:?}"
                    )));
                }
            };
            let (alias, column_aliases) = compile_alias(rf.alias.as_ref());
            Ok(FromClause::Function {
                name,
                args,
                alias,
                column_aliases,
            })
        }
        other => Err(SQLError::Unsupported(format!("FROM form: {other:?}"))),
    }
}

fn right_is_lateral(node: &Node) -> bool {
    match node.node.as_ref() {
        Some(NodeEnum::RangeSubselect(rs)) => rs.lateral,
        Some(NodeEnum::RangeFunction(rf)) => rf.lateral,
        _ => false,
    }
}

fn compile_alias(alias: Option<&pg_query::protobuf::Alias>) -> (Option<String>, Vec<String>) {
    let Some(a) = alias else {
        return (None, Vec::new());
    };
    let name = if a.aliasname.is_empty() {
        None
    } else {
        Some(a.aliasname.clone())
    };
    let cols: Vec<String> = a
        .colnames
        .iter()
        .filter_map(|n| extract_string(n).ok())
        .collect();
    (name, cols)
}

fn compile_select(stmt: &pg_query::protobuf::SelectStmt) -> Result<SelectStmt> {
    let from = match stmt.from_clause.first() {
        Some(node) => Some(compile_from_node(node)?),
        None => None,
    };
    let projections = compile_projections(&stmt.target_list)?;
    let r#where = stmt
        .where_clause
        .as_ref()
        .map(|w| compile_expr(w))
        .transpose()?;
    let order_by = compile_order_by(&stmt.sort_clause)?;
    let limit = compile_limit_offset_expr(stmt.limit_count.as_deref())?;
    let offset = compile_limit_offset_expr(stmt.limit_offset.as_deref())?;
    let (group_by, grouping_sets) = compile_group_clause(&stmt.group_clause)?;
    // Resolve GROUP BY 1 / GROUP BY <alias> against the SELECT list.
    // Postgres prefers a real column when one matches, falling back to
    // the alias; we don't have schema info here, so we only rewrite
    // when the alias clearly cannot be a column on the source row
    // (i.e., the projection's expression is something other than a
    // bare reference to that same name).
    let group_by = resolve_group_by_aliases(group_by, &projections);
    let grouping_sets: Vec<Vec<Expr>> = grouping_sets
        .into_iter()
        .map(|s| resolve_group_by_aliases(s, &projections))
        .collect();
    let having = stmt
        .having_clause
        .as_ref()
        .map(|h| compile_expr(h))
        .transpose()?;
    let with = match stmt.with_clause.as_ref() {
        Some(wc) => compile_with_clause(wc)?,
        None => Vec::new(),
    };
    let mut set_op = compile_set_op(stmt)?;

    // For UNION / INTERSECT / EXCEPT shapes the outer SelectStmt carries:
    //   * its own `sortClause` / `limitCount` / `limitOffset` -> the
    //     *combined* ORDER BY / LIMIT / OFFSET applied to `lhs <op> rhs`
    //     (those land on `set_op.combined_*`).
    //   * empty `targetList` / `fromClause`; the LHS branch (with its
    //     own clauses, including its own optional `ORDER BY` / `LIMIT`)
    //     lives in `stmt.larg`. We pull the LHS into the parent so the
    //     executor sees `SelectStmt { ..lhs.., set_op: Some(..) }`.
    let (projections, from, r#where, group_by, order_by, limit, offset) =
        if set_op.is_some() && stmt.larg.is_some() {
            // Promote the outer (combined) clauses onto the SetOp and
            // replace the parent's clauses with the LHS branch's.
            if let Some(so) = set_op.as_mut() {
                so.combined_order_by = order_by;
                so.combined_limit = limit;
                so.combined_offset = offset;
            }
            let lhs = compile_select(stmt.larg.as_deref().unwrap())?;
            (
                lhs.projections,
                lhs.from,
                lhs.r#where,
                lhs.group_by,
                lhs.order_by,
                lhs.limit,
                lhs.offset,
            )
        } else {
            (
                projections,
                from,
                r#where,
                group_by,
                order_by,
                limit,
                offset,
            )
        };

    let distinct = !stmt.distinct_clause.is_empty();

    Ok(SelectStmt {
        projections,
        from,
        r#where,
        group_by,
        grouping_sets,
        having,
        order_by,
        limit,
        offset,
        with,
        set_op,
        distinct,
    })
}

fn resolve_group_by_aliases(group_by: Vec<Expr>, projections: &[Projection]) -> Vec<Expr> {
    group_by
        .into_iter()
        .map(|g| match &g {
            // GROUP BY <ordinal>: refers to the Nth projection.
            Expr::Literal(Value::Int(n)) if *n >= 1 && (*n as usize) <= projections.len() => {
                projections[(*n as usize) - 1].expr.clone()
            }
            // GROUP BY <alias>: only rewrite when the alias points at
            // a non-trivial expression. If the projection is just a
            // column reference with the same name the original AST is
            // already correct.
            Expr::Column(name) => {
                for p in projections {
                    if let Some(alias) = &p.alias {
                        if alias == name {
                            if let Expr::Column(col_name) = &p.expr {
                                if col_name == name {
                                    return g;
                                }
                            }
                            return p.expr.clone();
                        }
                    }
                }
                g
            }
            _ => g,
        })
        .collect()
}

fn compile_group_clause(nodes: &[pg_query::protobuf::Node]) -> Result<(Vec<Expr>, Vec<Vec<Expr>>)> {
    use pg_query::protobuf::GroupingSetKind;
    let mut plain: Vec<Expr> = Vec::new();
    let mut sets: Vec<Vec<Expr>> = Vec::new();
    let mut has_grouping_set = false;
    for n in nodes {
        let Some(inner) = n.node.as_ref() else {
            continue;
        };
        if let NodeEnum::GroupingSet(gs) = inner {
            has_grouping_set = true;
            let kind = gs.kind();
            // The content list holds either column refs or nested
            // GroupingSet nodes (for nested ROLLUP / CUBE).
            let inner_exprs: Vec<Expr> = gs
                .content
                .iter()
                .filter_map(|c| compile_expr(c).ok())
                .collect();
            match kind {
                GroupingSetKind::GroupingSetEmpty => {
                    sets.push(Vec::new());
                }
                GroupingSetKind::GroupingSetSimple => {
                    sets.push(inner_exprs);
                }
                GroupingSetKind::GroupingSetRollup => {
                    // ROLLUP(a, b, c) -> (a, b, c), (a, b), (a), ()
                    let n = inner_exprs.len();
                    for i in (0..=n).rev() {
                        sets.push(inner_exprs[..i].to_vec());
                    }
                }
                GroupingSetKind::GroupingSetCube => {
                    // CUBE(a, b) -> all 2^n subsets.
                    let n = inner_exprs.len();
                    for mask in 0_usize..(1 << n) {
                        let mut s: Vec<Expr> = Vec::new();
                        for (i, e) in inner_exprs.iter().enumerate() {
                            if mask & (1 << i) != 0 {
                                s.push(e.clone());
                            }
                        }
                        sets.push(s);
                    }
                }
                GroupingSetKind::GroupingSetSets => {
                    // Explicit GROUPING SETS ((a, b), (a), ()): every
                    // child of `content` is itself a GroupingSet.
                    for child in &gs.content {
                        if let Some(NodeEnum::GroupingSet(child_gs)) = child.node.as_ref() {
                            let exprs: Vec<Expr> = child_gs
                                .content
                                .iter()
                                .filter_map(|c| compile_expr(c).ok())
                                .collect();
                            sets.push(exprs);
                        }
                    }
                }
                _ => {}
            }
        } else {
            plain.push(compile_expr(n)?);
        }
    }
    if !has_grouping_set {
        return Ok((plain, Vec::new()));
    }
    // Standard plain group-by columns are AND-merged with every
    // grouping set: each set acquires the plain prefix.
    let merged: Vec<Vec<Expr>> = if plain.is_empty() {
        sets
    } else {
        sets.into_iter()
            .map(|s| {
                let mut combined = plain.clone();
                combined.extend(s);
                combined
            })
            .collect()
    };
    Ok((Vec::new(), merged))
}

fn compile_projections(targets: &[pg_query::protobuf::Node]) -> Result<Vec<Projection>> {
    let mut out = Vec::with_capacity(targets.len());
    for target_node in targets {
        let Some(inner) = target_node.node.as_ref() else {
            continue;
        };
        let res_target = match inner {
            NodeEnum::ResTarget(t) => t,
            _ => return Err(SQLError::Internal(format!("unexpected target {inner:?}"))),
        };
        let alias = if res_target.name.is_empty() {
            None
        } else {
            Some(res_target.name.clone())
        };
        let expr = match &res_target.val {
            Some(node) => compile_expr(node)?,
            None => return Err(SQLError::Internal("ResTarget without value".into())),
        };
        out.push(Projection { expr, alias });
    }
    Ok(out)
}

fn compile_order_by(sort_clause: &[pg_query::protobuf::Node]) -> Result<Vec<OrderBy>> {
    let mut out = Vec::with_capacity(sort_clause.len());
    for sort_node in sort_clause {
        let Some(inner) = sort_node.node.as_ref() else {
            continue;
        };
        if let NodeEnum::SortBy(sb) = inner {
            let expr_node = sb
                .node
                .as_ref()
                .ok_or_else(|| SQLError::Internal("SortBy without expr".into()))?;
            let expr = compile_expr(expr_node)?;
            // SortByDir: SortbyDefault = 0, SortbyAsc = 2, SortbyDesc = 3,
            // SortbyUsing = 4 (per libpg_query 6.x).
            let descending = sb.sortby_dir == pg_query::protobuf::SortByDir::SortbyDesc as i32;
            // SortByNulls: SortbyNullsDefault = 0, SortbyNullsFirst = 1,
            // SortbyNullsLast = 2.
            // pg_query enum values: SortbyNullsDefault=1, First=2, Last=3.
            let nulls = match sb.sortby_nulls {
                2 => Some(crate::ast::NullsOrder::First),
                3 => Some(crate::ast::NullsOrder::Last),
                _ => None,
            };
            out.push(OrderBy {
                expr,
                descending,
                nulls,
            });
        }
    }
    Ok(out)
}

fn compile_set_op(stmt: &pg_query::protobuf::SelectStmt) -> Result<Option<Box<SetOp>>> {
    let kind = match stmt.op() {
        pg_query::protobuf::SetOperation::SetopNone => return Ok(None),
        pg_query::protobuf::SetOperation::SetopUnion => SetOpKind::Union,
        pg_query::protobuf::SetOperation::SetopIntersect => SetOpKind::Intersect,
        pg_query::protobuf::SetOperation::SetopExcept => SetOpKind::Except,
        other => return Err(SQLError::Unsupported(format!("set op {other:?}"))),
    };
    let right_node = stmt
        .rarg
        .as_deref()
        .ok_or_else(|| SQLError::Internal("set op missing right".into()))?;
    let right = compile_select(right_node)?;
    Ok(Some(Box::new(SetOp {
        kind,
        all: stmt.all,
        right,
        // The outer SelectStmt's ORDER BY / LIMIT / OFFSET land here
        // when `compile_select` finishes — the caller fills these in
        // because at this point we don't have the parent's clauses
        // resolved yet. Default to empty / None until then.
        combined_order_by: Vec::new(),
        combined_limit: None,
        combined_offset: None,
    })))
}

fn compile_with_clause(wc: &pg_query::protobuf::WithClause) -> Result<Vec<CTE>> {
    let mut out = Vec::with_capacity(wc.ctes.len());
    for cte_node in &wc.ctes {
        let Some(inner) = cte_node.node.as_ref() else {
            continue;
        };
        let cte = match inner {
            NodeEnum::CommonTableExpr(c) => c,
            _ => return Err(SQLError::Internal("expected CommonTableExpr".into())),
        };
        let select_node = cte
            .ctequery
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CTE without query".into()))?;
        let select_inner = select_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CTE query node empty".into()))?;
        let select = match select_inner {
            NodeEnum::SelectStmt(s) => s,
            _ => return Err(SQLError::Unsupported("CTE body must be SELECT".into())),
        };
        out.push(CTE {
            name: cte.ctename.clone(),
            recursive: wc.recursive,
            query: Box::new(compile_select(select)?),
        });
    }
    Ok(out)
}

/// Compile a `LIMIT` / `OFFSET` operand into an [`Expr`]. The
/// expression is resolved to an integer at execute time, so `LIMIT $1`
/// and other parameter-bearing forms work end-to-end. `None` means the
/// clause was absent entirely (`SELECT ... LIMIT NULL` is also `None`
/// because PG treats `NULL` as "no limit").
fn compile_limit_offset_expr(node: Option<&Node>) -> Result<Option<Expr>> {
    use pg_query::protobuf::a_const::Val;
    let Some(node) = node else { return Ok(None) };
    let Some(inner) = node.node.as_ref() else {
        return Ok(None);
    };
    // `SELECT ... LIMIT NULL` parses as an `AConst` with no `val` --
    // treat it like an absent clause.
    if let NodeEnum::AConst(c) = inner {
        if c.val.is_none() {
            return Ok(None);
        }
        if let Some(Val::Ival(i)) = &c.val {
            if i.ival < 0 {
                return Err(SQLError::Internal("negative LIMIT/OFFSET".into()));
            }
        }
    }
    Ok(Some(compile_expr(node)?))
}

// -------------------------------------------------------------------------
// Expression compiler
// -------------------------------------------------------------------------

fn compile_expr(node: &Node) -> Result<Expr> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("missing expr node".into()));
    };
    match inner {
        NodeEnum::AConst(c) => compile_const(c),
        NodeEnum::ColumnRef(c) => compile_column_ref(c),
        NodeEnum::ParamRef(p) => Ok(Expr::Param(p.number as usize)),
        NodeEnum::FuncCall(f) => compile_func_call(f),
        NodeEnum::AArrayExpr(a) => {
            let elements: Vec<Expr> = a
                .elements
                .iter()
                .map(compile_expr)
                .collect::<Result<Vec<_>>>()?;
            Ok(Expr::Array(elements))
        }
        NodeEnum::TypeCast(tc) => compile_type_cast(tc),
        NodeEnum::AExpr(a) => compile_a_expr(a),
        NodeEnum::BoolExpr(b) => compile_bool_expr(b),
        NodeEnum::NullTest(n) => compile_null_test(n),
        NodeEnum::CaseExpr(c) => compile_case_expr(c),
        NodeEnum::CoalesceExpr(ce) => {
            let args: Vec<Expr> = ce
                .args
                .iter()
                .map(compile_expr)
                .collect::<Result<Vec<_>>>()?;
            Ok(Expr::Func {
                name: "coalesce".into(),
                args,
            distinct: false, order_by: Vec::new(), filter: None })
        }
        NodeEnum::MinMaxExpr(me) => {
            use pg_query::protobuf::MinMaxOp;
            let name = match me.op() {
                MinMaxOp::IsGreatest => "greatest",
                MinMaxOp::IsLeast => "least",
                _ => {
                    return Err(SQLError::Unsupported(format!(
                        "MinMaxExpr op {:?}",
                        me.op()
                    )));
                }
            };
            let args: Vec<Expr> = me
                .args
                .iter()
                .map(compile_expr)
                .collect::<Result<Vec<_>>>()?;
            Ok(Expr::Func {
                name: name.into(),
                args,
            distinct: false, order_by: Vec::new(), filter: None })
        }
        NodeEnum::SubLink(sl) => compile_sublink(sl),
        other => Err(SQLError::Unsupported(format!("expression form: {other:?}"))),
    }
}

fn compile_sublink(sl: &pg_query::protobuf::SubLink) -> Result<Expr> {
    use pg_query::protobuf::SubLinkType;
    let body_node = sl
        .subselect
        .as_deref()
        .ok_or_else(|| SQLError::Internal("SubLink without subselect".into()))?;
    let inner_select = match body_node.node.as_ref() {
        Some(NodeEnum::SelectStmt(s)) => compile_select(s)?,
        _ => {
            return Err(SQLError::Unsupported("SubLink body must be SELECT".into()));
        }
    };
    let body = Box::new(inner_select);
    match sl.sub_link_type() {
        SubLinkType::ExprSublink => Ok(Expr::ScalarSubquery(body)),
        SubLinkType::ExistsSublink => Ok(Expr::Exists {
            body,
            negated: false,
        }),
        SubLinkType::AnySublink => {
            let testexpr = sl
                .testexpr
                .as_deref()
                .ok_or_else(|| SQLError::Internal("ANY SubLink without testexpr".into()))?;
            Ok(Expr::InSubquery {
                expr: Box::new(compile_expr(testexpr)?),
                body,
                negated: false,
            })
        }
        SubLinkType::AllSublink => {
            // ALL is the negation of ANY <> for the dual operator. We
            // promote to InSubquery semantics with a clear marker; the
            // evaluator treats ALL like NOT IN for equality.
            let testexpr = sl
                .testexpr
                .as_deref()
                .ok_or_else(|| SQLError::Internal("ALL SubLink without testexpr".into()))?;
            Ok(Expr::InSubquery {
                expr: Box::new(compile_expr(testexpr)?),
                body,
                negated: true,
            })
        }
        other => Err(SQLError::Unsupported(format!("SubLink type {other:?}"))),
    }
}

fn compile_case_expr(c: &pg_query::protobuf::CaseExpr) -> Result<Expr> {
    let base = c
        .arg
        .as_ref()
        .map(|n| compile_expr(n))
        .transpose()?
        .map(Box::new);
    let mut when: Vec<(Expr, Expr)> = Vec::with_capacity(c.args.len());
    for arm in &c.args {
        let inner = arm
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CASE arm without body".into()))?;
        let NodeEnum::CaseWhen(cw) = inner else {
            return Err(SQLError::Internal(format!(
                "CASE arm expected CaseWhen, got {inner:?}"
            )));
        };
        let cond = cw
            .expr
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CASE WHEN without cond".into()))?;
        let result = cw
            .result
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CASE WHEN without THEN".into()))?;
        when.push((compile_expr(cond)?, compile_expr(result)?));
    }
    let else_branch = c
        .defresult
        .as_ref()
        .map(|n| compile_expr(n))
        .transpose()?
        .map(Box::new);
    Ok(Expr::Case {
        base,
        when,
        else_branch,
    })
}

fn compile_a_expr(a: &pg_query::protobuf::AExpr) -> Result<Expr> {
    use pg_query::protobuf::AExprKind;
    let kind = a.kind();
    match kind {
        AExprKind::AexprOp => {
            let op_name = a
                .name
                .iter()
                .filter_map(|n| extract_string(n).ok())
                .collect::<Vec<_>>()
                .join("");
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("AExpr missing lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("AExpr missing rhs".into()))?;
            let op = match op_name.as_str() {
                "=" => BinaryOp::Equal,
                "<>" | "!=" => BinaryOp::NotEqual,
                "<" => BinaryOp::Less,
                "<=" => BinaryOp::LessEqual,
                ">" => BinaryOp::Greater,
                ">=" => BinaryOp::GreaterEqual,
                "+" => BinaryOp::Add,
                "-" => BinaryOp::Subtract,
                "*" => BinaryOp::Multiply,
                "/" => BinaryOp::Divide,
                // String concatenation: rewrite `a || b` into a
                // concat_op() call. concat_op propagates NULL the way
                // the SQL `||` operator must (`'x' || NULL == NULL`),
                // which is distinct from PostgreSQL's `CONCAT()` that
                // skips NULL arguments.
                "||" => {
                    return Ok(Expr::Func {
                        name: "concat_op".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                    distinct: false, order_by: Vec::new(), filter: None });
                }
                "%" => {
                    return Ok(Expr::Func {
                        name: "mod".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                    distinct: false, order_by: Vec::new(), filter: None });
                }
                "~~" => {
                    return Ok(Expr::Func {
                        name: "like".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                    distinct: false, order_by: Vec::new(), filter: None });
                }
                "~~*" => {
                    return Ok(Expr::Func {
                        name: "ilike".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                    distinct: false, order_by: Vec::new(), filter: None });
                }
                "!~~" => {
                    return Ok(Expr::Not(Box::new(Expr::Func {
                        name: "like".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                    distinct: false, order_by: Vec::new(), filter: None })));
                }
                "!~~*" => {
                    return Ok(Expr::Not(Box::new(Expr::Func {
                        name: "ilike".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                    distinct: false, order_by: Vec::new(), filter: None })));
                }
                "->" => {
                    return Ok(Expr::Func {
                        name: "json_extract_path".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                    distinct: false, order_by: Vec::new(), filter: None });
                }
                "->>" => {
                    return Ok(Expr::Func {
                        name: "json_extract_path_text".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                    distinct: false, order_by: Vec::new(), filter: None });
                }
                "#>" => {
                    return Ok(Expr::Func {
                        name: "json_extract_path".into(),
                        args: json_path_args(compile_expr(lhs)?, compile_expr(rhs)?),
                    distinct: false, order_by: Vec::new(), filter: None });
                }
                "#>>" => {
                    return Ok(Expr::Func {
                        name: "json_extract_path_text".into(),
                        args: json_path_args(compile_expr(lhs)?, compile_expr(rhs)?),
                    distinct: false, order_by: Vec::new(), filter: None });
                }
                other => return Err(SQLError::Unsupported(format!("operator `{other}`"))),
            };
            Ok(Expr::Binary {
                op,
                lhs: Box::new(compile_expr(lhs)?),
                rhs: Box::new(compile_expr(rhs)?),
            })
        }
        AExprKind::AexprBetween | AExprKind::AexprNotBetween => {
            let expr = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("BETWEEN without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("BETWEEN without rhs".into()))?;
            let bounds = match rhs.node.as_ref() {
                Some(NodeEnum::List(l)) if l.items.len() == 2 => l.items.clone(),
                _ => return Err(SQLError::Internal("BETWEEN expects 2 bounds".into())),
            };
            let between = Expr::Between {
                expr: Box::new(compile_expr(expr)?),
                low: Box::new(compile_expr(&bounds[0])?),
                high: Box::new(compile_expr(&bounds[1])?),
            };
            Ok(if matches!(kind, AExprKind::AexprNotBetween) {
                Expr::Not(Box::new(between))
            } else {
                between
            })
        }
        AExprKind::AexprNullif => {
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("NULLIF without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("NULLIF without rhs".into()))?;
            return Ok(Expr::Func {
                name: "nullif".into(),
                args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
            distinct: false, order_by: Vec::new(), filter: None });
        }
        AExprKind::AexprLike => {
            // libpg_query encodes LIKE as `~~` and NOT LIKE as `!~~` in
            // `a.name`. The keyword form lands here regardless of the
            // user's syntax (LIKE / NOT LIKE / ~~ / !~~), so we have to
            // peek at the name to recover the negation.
            let op_name = a
                .name
                .iter()
                .filter_map(|n| extract_string(n).ok())
                .collect::<Vec<_>>()
                .join("");
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("LIKE without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("LIKE without rhs".into()))?;
            let func = Expr::Func {
                name: "like".into(),
                args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
            distinct: false, order_by: Vec::new(), filter: None };
            return Ok(if op_name == "!~~" {
                Expr::Not(Box::new(func))
            } else {
                func
            });
        }
        AExprKind::AexprIlike => {
            // Same shape as AexprLike: ILIKE -> `~~*`, NOT ILIKE -> `!~~*`.
            let op_name = a
                .name
                .iter()
                .filter_map(|n| extract_string(n).ok())
                .collect::<Vec<_>>()
                .join("");
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("ILIKE without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("ILIKE without rhs".into()))?;
            let func = Expr::Func {
                name: "ilike".into(),
                args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
            distinct: false, order_by: Vec::new(), filter: None };
            return Ok(if op_name == "!~~*" {
                Expr::Not(Box::new(func))
            } else {
                func
            });
        }
        AExprKind::AexprIn => {
            let expr = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("IN without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("IN without rhs".into()))?;
            let items = match rhs.node.as_ref() {
                Some(NodeEnum::List(l)) => l.items.clone(),
                _ => return Err(SQLError::Internal("IN expects list".into())),
            };
            let list: Vec<Expr> = items.iter().map(compile_expr).collect::<Result<Vec<_>>>()?;
            let negated = a
                .name
                .first()
                .and_then(|n| n.node.as_ref())
                .and_then(|inner| match inner {
                    NodeEnum::String(s) => Some(s.sval == "<>"),
                    _ => None,
                })
                .unwrap_or(false);
            Ok(Expr::InList {
                expr: Box::new(compile_expr(expr)?),
                list,
                negated,
            })
        }
        other => Err(SQLError::Unsupported(format!("AExpr kind: {other:?}"))),
    }
}

fn compile_bool_expr(b: &pg_query::protobuf::BoolExpr) -> Result<Expr> {
    use pg_query::protobuf::BoolExprType;
    let kind = b.boolop();
    let args: Vec<Expr> = b
        .args
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    match kind {
        BoolExprType::AndExpr => Ok(Expr::And(args)),
        BoolExprType::OrExpr => Ok(Expr::Or(args)),
        BoolExprType::NotExpr => {
            let arg = args
                .into_iter()
                .next()
                .ok_or_else(|| SQLError::Internal("NOT without operand".into()))?;
            Ok(Expr::Not(Box::new(arg)))
        }
        _ => Err(SQLError::Unsupported(format!("BoolExpr {kind:?}"))),
    }
}

fn compile_null_test(n: &pg_query::protobuf::NullTest) -> Result<Expr> {
    use pg_query::protobuf::NullTestType;
    let arg = n
        .arg
        .as_ref()
        .ok_or_else(|| SQLError::Internal("NullTest without arg".into()))?;
    let negated = matches!(n.nulltesttype(), NullTestType::IsNotNull);
    Ok(Expr::IsNull {
        expr: Box::new(compile_expr(arg)?),
        negated,
    })
}

fn compile_const(c: &pg_query::protobuf::AConst) -> Result<Expr> {
    if c.isnull {
        return Ok(Expr::Literal(Value::Null));
    }
    use pg_query::protobuf::a_const::Val;
    let Some(val) = c.val.as_ref() else {
        return Ok(Expr::Literal(Value::Null));
    };
    let value = match val {
        Val::Ival(i) => Value::Int(i64::from(i.ival)),
        Val::Fval(f) => Value::Float(
            f.fval
                .parse::<f64>()
                .map_err(|e| SQLError::Internal(e.to_string()))?,
        ),
        Val::Sval(s) => Value::Str(s.sval.clone()),
        Val::Boolval(b) => Value::Bool(b.boolval),
        other => {
            return Err(SQLError::Unsupported(format!("constant: {other:?}")));
        }
    };
    Ok(Expr::Literal(value))
}

fn compile_column_ref(c: &pg_query::protobuf::ColumnRef) -> Result<Expr> {
    let mut parts: Vec<String> = Vec::new();
    for f in &c.fields {
        let Some(inner) = f.node.as_ref() else {
            continue;
        };
        match inner {
            NodeEnum::String(s) => parts.push(s.sval.clone()),
            NodeEnum::AStar(_) => return Ok(Expr::Star),
            _ => {}
        }
    }
    match parts.len() {
        0 => Err(SQLError::Internal("empty ColumnRef".into())),
        1 => Ok(Expr::Column(parts.pop().unwrap())),
        _ => {
            // `schema.table.col` collapses to `table.col`; `t.col`
            // round-trips as a qualified ref.
            let column = parts.pop().unwrap();
            let qualifier = parts.pop().unwrap();
            Ok(Expr::QualifiedColumn { qualifier, column })
        }
    }
}

fn compile_func_call(f: &pg_query::protobuf::FuncCall) -> Result<Expr> {
    let raw_name = f
        .funcname
        .iter()
        .filter_map(|n| {
            n.node.as_ref().and_then(|inner| match inner {
                NodeEnum::String(s) => Some(s.sval.clone()),
                _ => None,
            })
        })
        .collect::<Vec<_>>()
        .last()
        .cloned()
        .unwrap_or_default();
    let mut args = f
        .args
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    if let Some(over) = f.over.as_ref() {
        let spec = compile_window_spec(over)?;
        return Ok(Expr::WindowCall {
            name: raw_name,
            args,
            spec,
        });
    }
    // COUNT(*): the parser leaves `args` empty; mark explicitly so
    // the dispatcher distinguishes it from COUNT(column).
    if f.agg_star && args.is_empty() {
        args.push(Expr::Star);
    }
    // Translate the aggregate's ORDER BY clauses (e.g.
    // `string_agg(name, ',' ORDER BY name)`) into typed `OrderBy`
    // entries on `Expr::Func.order_by`.
    let mut agg_order: Vec<OrderBy> = Vec::new();
    for sort_node in &f.agg_order {
        let Some(inner) = sort_node.node.as_ref() else {
            continue;
        };
        if let NodeEnum::SortBy(sb) = inner {
            let expr_node = sb
                .node
                .as_ref()
                .ok_or_else(|| SQLError::Internal("agg_order SortBy without expr".into()))?;
            let key_expr = compile_expr(expr_node)?;
            let descending = sb.sortby_dir == pg_query::protobuf::SortByDir::SortbyDesc as i32;
            let nulls = match sb.sortby_nulls {
                2 => Some(crate::ast::NullsOrder::First),
                3 => Some(crate::ast::NullsOrder::Last),
                _ => None,
            };
            agg_order.push(OrderBy {
                expr: key_expr,
                descending,
                nulls,
            });
        }
    }
    let agg_filter = match f.agg_filter.as_ref() {
        Some(inner) => Some(Box::new(compile_expr(inner)?)),
        None => None,
    };
    Ok(Expr::Func {
        name: raw_name,
        args,
        distinct: f.agg_distinct,
        order_by: agg_order,
        filter: agg_filter,
    })
}

fn compile_window_spec(w: &pg_query::protobuf::WindowDef) -> Result<WindowSpec> {
    let partition_by: Vec<Expr> = w
        .partition_clause
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    let mut order_by = Vec::new();
    for sort_node in &w.order_clause {
        let Some(inner) = sort_node.node.as_ref() else {
            continue;
        };
        if let NodeEnum::SortBy(sb) = inner {
            let expr_node = sb
                .node
                .as_ref()
                .ok_or_else(|| SQLError::Internal("SortBy without expr".into()))?;
            let expr = compile_expr(expr_node)?;
            let descending = sb.sortby_dir == pg_query::protobuf::SortByDir::SortbyDesc as i32;
            // pg_query enum values: SortbyNullsDefault=1, First=2, Last=3.
            let nulls = match sb.sortby_nulls {
                2 => Some(crate::ast::NullsOrder::First),
                3 => Some(crate::ast::NullsOrder::Last),
                _ => None,
            };
            order_by.push(OrderBy {
                expr,
                descending,
                nulls,
            });
        }
    }
    let frame = compile_window_frame(w)?;
    Ok(WindowSpec {
        partition_by,
        order_by,
        frame,
    })
}

fn compile_window_frame(
    w: &pg_query::protobuf::WindowDef,
) -> Result<Option<crate::ast::WindowFrame>> {
    use crate::ast::{FrameBound, FrameMode, WindowFrame};
    if w.frame_options == 0 {
        return Ok(None);
    }
    // pg_query bit constants for frame_options.
    const FRAMEOPTION_NONDEFAULT: u32 = 0x000_0001;
    const FRAMEOPTION_ROWS: u32 = 0x000_0004;
    const FRAMEOPTION_GROUPS: u32 = 0x000_0008;
    const FRAMEOPTION_BETWEEN: u32 = 0x000_0010;
    const FRAMEOPTION_START_UNBOUNDED_PRECEDING: u32 = 0x000_0020;
    const FRAMEOPTION_END_UNBOUNDED_PRECEDING: u32 = 0x000_0040;
    const FRAMEOPTION_START_UNBOUNDED_FOLLOWING: u32 = 0x000_0080;
    const FRAMEOPTION_END_UNBOUNDED_FOLLOWING: u32 = 0x000_0100;
    const FRAMEOPTION_START_CURRENT_ROW: u32 = 0x000_0200;
    const FRAMEOPTION_END_CURRENT_ROW: u32 = 0x000_0400;
    const FRAMEOPTION_START_OFFSET_PRECEDING: u32 = 0x000_0800;
    const FRAMEOPTION_END_OFFSET_PRECEDING: u32 = 0x000_1000;
    const FRAMEOPTION_START_OFFSET_FOLLOWING: u32 = 0x000_2000;
    const FRAMEOPTION_END_OFFSET_FOLLOWING: u32 = 0x000_4000;
    let f = w.frame_options as u32;
    let _ = FRAMEOPTION_BETWEEN;
    // PostgreSQL always encodes a default frame in `frame_options`
    // (RANGE UNBOUNDED PRECEDING TO CURRENT ROW). Only honor the
    // frame when the user explicitly wrote one — that's exactly what
    // the `FRAMEOPTION_NONDEFAULT` bit indicates.
    if f & FRAMEOPTION_NONDEFAULT == 0 {
        return Ok(None);
    }
    let mode = if f & FRAMEOPTION_ROWS != 0 {
        FrameMode::Rows
    } else if f & FRAMEOPTION_GROUPS != 0 {
        FrameMode::Groups
    } else {
        // FRAMEOPTION_RANGE is the default mode bit when neither ROWS
        // nor GROUPS is set; an unset flag also defaults to RANGE.
        FrameMode::Range
    };
    let start = if f & FRAMEOPTION_START_UNBOUNDED_PRECEDING != 0 {
        FrameBound::UnboundedPreceding
    } else if f & FRAMEOPTION_START_UNBOUNDED_FOLLOWING != 0 {
        FrameBound::UnboundedFollowing
    } else if f & FRAMEOPTION_START_CURRENT_ROW != 0 {
        FrameBound::CurrentRow
    } else if f & FRAMEOPTION_START_OFFSET_PRECEDING != 0 {
        let n = w
            .start_offset
            .as_deref()
            .ok_or_else(|| SQLError::Internal("PRECEDING without offset".into()))?;
        FrameBound::Preceding(Box::new(compile_expr(n)?))
    } else if f & FRAMEOPTION_START_OFFSET_FOLLOWING != 0 {
        let n = w
            .start_offset
            .as_deref()
            .ok_or_else(|| SQLError::Internal("FOLLOWING without offset".into()))?;
        FrameBound::Following(Box::new(compile_expr(n)?))
    } else {
        FrameBound::UnboundedPreceding
    };
    let end = if f & FRAMEOPTION_END_UNBOUNDED_PRECEDING != 0 {
        FrameBound::UnboundedPreceding
    } else if f & FRAMEOPTION_END_UNBOUNDED_FOLLOWING != 0 {
        FrameBound::UnboundedFollowing
    } else if f & FRAMEOPTION_END_CURRENT_ROW != 0 {
        FrameBound::CurrentRow
    } else if f & FRAMEOPTION_END_OFFSET_PRECEDING != 0 {
        let n = w
            .end_offset
            .as_deref()
            .ok_or_else(|| SQLError::Internal("PRECEDING without offset".into()))?;
        FrameBound::Preceding(Box::new(compile_expr(n)?))
    } else if f & FRAMEOPTION_END_OFFSET_FOLLOWING != 0 {
        let n = w
            .end_offset
            .as_deref()
            .ok_or_else(|| SQLError::Internal("FOLLOWING without offset".into()))?;
        FrameBound::Following(Box::new(compile_expr(n)?))
    } else {
        FrameBound::CurrentRow
    };
    Ok(Some(WindowFrame { mode, start, end }))
}

fn compile_type_cast(tc: &pg_query::protobuf::TypeCast) -> Result<Expr> {
    let arg = tc
        .arg
        .as_ref()
        .ok_or_else(|| SQLError::Internal("TypeCast without arg".into()))?;
    let inner = compile_expr(arg)?;
    let raw_names: Vec<String> = tc
        .type_name
        .as_ref()
        .map(|t| {
            t.names
                .iter()
                .filter_map(|n| extract_string(n).ok())
                .collect()
        })
        .unwrap_or_default();
    // libpg_query reports built-in types qualified as `pg_catalog.<name>`;
    // peel the schema off so the evaluator only ever sees the bare type
    // and treat aliases (`int4` -> `integer`, `float8` -> `double
    // precision`) up front.
    let ty = raw_names.last().cloned().unwrap_or_default().to_lowercase();
    let ty = match ty.as_str() {
        "int2" => "smallint".to_string(),
        "int4" => "integer".to_string(),
        "int8" => "bigint".to_string(),
        "float4" => "real".to_string(),
        "float8" => "double precision".to_string(),
        _ => ty,
    };
    if ty.is_empty() {
        return Ok(inner);
    }
    Ok(Expr::Cast {
        expr: Box::new(inner),
        ty,
    })
}

/// Convenience for tests that only need to round-trip through the
/// compiler without an Engine in scope.
pub fn plan_only_for_test(sql: &str) -> Result<Vec<Statement>> {
    compile(sql)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(sql: &str) -> Statement {
        let mut v = compile(sql).unwrap();
        assert_eq!(v.len(), 1, "expected 1 stmt");
        v.remove(0)
    }

    #[test]
    fn create_table_with_vector_column() {
        let stmt =
            first("CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, embedding VECTOR(4))");
        let Statement::CreateTable(ct) = stmt else {
            panic!("not CREATE TABLE");
        };
        assert_eq!(ct.name, "docs");
        assert_eq!(ct.columns.len(), 3);
        assert!(matches!(ct.columns[0].ty, ColumnType::Integer));
        assert!(ct.columns[0].primary_key);
        assert!(matches!(ct.columns[1].ty, ColumnType::Text));
        assert!(matches!(ct.columns[2].ty, ColumnType::Vector(4)));
    }

    #[test]
    fn create_index_records_access_method() {
        let stmt = first("CREATE INDEX idx_body ON docs USING gin (body)");
        let Statement::CreateIndex(ci) = stmt else {
            panic!("not CREATE INDEX");
        };
        assert_eq!(ci.table, "docs");
        assert_eq!(ci.access_method, "gin");
        assert_eq!(ci.columns, vec!["body"]);
    }

    #[test]
    fn insert_with_array_literal() {
        let stmt = first(
            "INSERT INTO docs (id, title, embedding) VALUES \
             (1, 'rust language', ARRAY[0.1, 0.2, 0.3])",
        );
        let Statement::Insert(i) = stmt else {
            panic!("not INSERT");
        };
        assert_eq!(i.table, "docs");
        assert_eq!(i.columns, vec!["id", "title", "embedding"]);
        assert_eq!(i.rows.len(), 1);
        assert_eq!(i.rows[0].len(), 3);
        match &i.rows[0][2] {
            Expr::Array(v) => assert_eq!(v.len(), 3),
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn select_with_function_call_and_order_by() {
        let stmt = first(
            "SELECT id, title, _score AS s FROM docs \
             WHERE text_match(body, 'rust language') \
             ORDER BY _score DESC LIMIT 5",
        );
        let Statement::Select(s) = stmt else {
            panic!("not SELECT");
        };
        assert_eq!(s.projections.len(), 3);
        assert_eq!(s.projections[2].alias.as_deref(), Some("s"));
        match &s.from {
            Some(FromClause::Table { name, .. }) => assert_eq!(name, "docs"),
            other => panic!("expected single-table FROM, got {other:?}"),
        }
        assert!(matches!(s.r#where, Some(Expr::Func { .. , distinct: false, order_by: Vec::new(), filter: None })));
        assert_eq!(s.order_by.len(), 1);
        assert!(s.order_by[0].descending);
        match &s.limit {
            Some(Expr::Literal(uqa_core::Value::Int(5))) => {}
            other => panic!("expected LIMIT 5, got {other:?}"),
        }
    }
}
