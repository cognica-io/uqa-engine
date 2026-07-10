//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lift a `pg_query` parse tree into the internal [`Statement`] AST.
//!
//! The compiler accepts `PostgreSQL` syntax through `pg_query` and lifts
//! supported statements into the internal AST. Syntax that parses but is
//! outside the current SQL surface compiles to [`SQLError::Unsupported`].

use crate::ast::{
    AlterTableAction, AlterTableStmt, ColumnDef, DeleteStmt, DropKind, DropStmt, Expr, Statement,
    TransactionStmt, UpdateStmt,
};
use crate::error::{Result, SQLError};
use pg_query::protobuf::{Node, RangeVar};
use pg_query::NodeEnum;
use types::compile_pg_type_name;

mod tree;
mod types;

use tree::{
    compile_column_def, compile_create_index, compile_create_table, compile_expr,
    compile_from_node, compile_insert, compile_projections, compile_select, compile_with_clause,
    extract_string,
};

pub(super) fn range_var_name(r: &RangeVar) -> String {
    if r.schemaname.is_empty() {
        r.relname.clone()
    } else {
        format!("{}.{}", r.schemaname, r.relname)
    }
}

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
        NodeEnum::DropStmt(stmt) => compile_drop(stmt),
        NodeEnum::AlterTableStmt(stmt) => compile_alter_table(stmt).map(Statement::AlterTable),
        NodeEnum::RenameStmt(stmt) => compile_rename(stmt).map(Statement::AlterTable),
        NodeEnum::ViewStmt(stmt) => compile_create_view(stmt),
        NodeEnum::CreateSchemaStmt(stmt) => compile_create_schema(stmt),
        NodeEnum::ExplainStmt(stmt) => compile_explain(stmt),
        NodeEnum::VacuumStmt(_) => {
            // Treat ANALYZE / VACUUM ANALYZE the same way: ask the
            // engine to refresh stats. The canonical UQA behavior parses
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
        .map(range_var_name)
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
        .map(range_var_name)
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
        .map(range_var_name)
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
        .map(range_var_name)
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

    let returning = compile_projections(&stmt.returning_list)?;
    Ok(MergeStmt {
        target,
        target_alias,
        source,
        join_condition,
        when_clauses,
        returning,
    })
}

pub(super) fn collect_def_elem_options(nodes: &[Node]) -> Vec<(String, String)> {
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
        .map(range_var_name)
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

/// Last non-`pg_catalog` segment of a `TypeName`, lower-cased, with
/// `%TYPE` and array-bound suffixes preserved so the executor can
/// treat them as uncastable (best-effort) types.
fn compile_function_type_name(t: &pg_query::protobuf::TypeName) -> String {
    let mut last = String::new();
    for n in &t.names {
        if let Some(NodeEnum::String(s)) = n.node.as_ref() {
            if s.sval != "pg_catalog" {
                last.clone_from(&s.sval);
            }
        }
    }
    // `setof` is inspected separately by the caller; the name itself
    // stays scalar.
    let mut name = last.trim().to_ascii_lowercase();
    if t.pct_type {
        name.push_str("%type");
    }
    for _ in &t.array_bounds {
        name.push_str("[]");
    }
    name
}

/// String payload of a `DefElem` argument.
fn def_elem_string(elem: &pg_query::protobuf::DefElem) -> Option<String> {
    match elem.arg.as_ref().and_then(|a| a.node.as_ref()) {
        Some(NodeEnum::String(s)) => Some(s.sval.clone()),
        _ => None,
    }
}

fn compile_create_function(
    stmt: &pg_query::protobuf::CreateFunctionStmt,
) -> Result<crate::ast::CreateFunction> {
    use crate::ast::{
        CreateFunction, FunctionBody, FunctionParam, FunctionParamMode, FunctionReturns,
        FunctionVolatility,
    };
    use pg_query::protobuf::FunctionParameterMode;

    let keyword = if stmt.is_procedure {
        "CREATE PROCEDURE"
    } else {
        "CREATE FUNCTION"
    };
    let name = stmt
        .funcname
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(NodeEnum::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .next_back()
        .ok_or_else(|| SQLError::Internal(format!("{keyword} without a name")))?
        .to_ascii_lowercase();

    let mut params: Vec<FunctionParam> = Vec::with_capacity(stmt.parameters.len());
    let mut has_table_param = false;
    for p in &stmt.parameters {
        let Some(NodeEnum::FunctionParameter(fp)) = p.node.as_ref() else {
            return Err(SQLError::Internal(format!(
                "{keyword}: malformed parameter"
            )));
        };
        let mode = match fp.mode() {
            FunctionParameterMode::FuncParamIn | FunctionParameterMode::FuncParamDefault => {
                FunctionParamMode::In
            }
            FunctionParameterMode::FuncParamOut => FunctionParamMode::Out,
            FunctionParameterMode::FuncParamInout => FunctionParamMode::InOut,
            FunctionParameterMode::FuncParamTable => {
                has_table_param = true;
                FunctionParamMode::Table
            }
            FunctionParameterMode::FuncParamVariadic => {
                return Err(SQLError::Unsupported(format!(
                    "{keyword}: VARIADIC parameters"
                )));
            }
            FunctionParameterMode::Undefined => {
                return Err(SQLError::Internal(format!(
                    "{keyword}: parameter mode missing"
                )));
            }
        };
        let type_name = fp
            .arg_type
            .as_ref()
            .map(compile_function_type_name)
            .ok_or_else(|| SQLError::Internal(format!("{keyword}: parameter without type")))?;
        let default = match fp.defexpr.as_ref() {
            Some(node) => Some(compile_expr(node)?),
            None => None,
        };
        params.push(FunctionParam {
            name: fp.name.to_ascii_lowercase(),
            type_name,
            mode,
            default,
        });
    }

    // Mirror PostgreSQL's parse-time rule: once an input parameter
    // has a DEFAULT, every following input parameter needs one too.
    let mut saw_default = false;
    for p in &params {
        if !matches!(p.mode, FunctionParamMode::In | FunctionParamMode::InOut) {
            continue;
        }
        if p.default.is_some() {
            saw_default = true;
        } else if saw_default {
            return Err(SQLError::Unsupported(
                "input parameters after one with a default value must also have defaults".into(),
            ));
        }
    }

    let returns = if has_table_param {
        FunctionReturns::Table
    } else {
        match stmt.return_type.as_ref() {
            None => FunctionReturns::None,
            Some(t) => {
                let type_name = compile_function_type_name(t);
                if t.setof {
                    FunctionReturns::SetOf { type_name }
                } else {
                    FunctionReturns::Scalar { type_name }
                }
            }
        }
    };

    let mut language = String::new();
    let mut volatility = FunctionVolatility::Volatile;
    let mut strict = false;
    let mut source: Option<String> = None;
    for opt in &stmt.options {
        let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() else {
            continue;
        };
        match elem.defname.to_ascii_lowercase().as_str() {
            "language" => {
                language = def_elem_string(elem)
                    .unwrap_or_default()
                    .to_ascii_lowercase();
            }
            "volatility" => {
                volatility = match def_elem_string(elem).unwrap_or_default().as_str() {
                    "immutable" => FunctionVolatility::Immutable,
                    "stable" => FunctionVolatility::Stable,
                    _ => FunctionVolatility::Volatile,
                };
            }
            "strict" => {
                strict = matches!(
                    elem.arg.as_ref().and_then(|a| a.node.as_ref()),
                    Some(NodeEnum::Boolean(b)) if b.boolval
                );
            }
            "as" => {
                let items: Vec<String> = match elem.arg.as_ref().and_then(|a| a.node.as_ref()) {
                    Some(NodeEnum::List(list)) => list
                        .items
                        .iter()
                        .filter_map(|n| match n.node.as_ref() {
                            Some(NodeEnum::String(s)) => Some(s.sval.clone()),
                            _ => None,
                        })
                        .collect(),
                    Some(NodeEnum::String(s)) => vec![s.sval.clone()],
                    _ => Vec::new(),
                };
                match items.len() {
                    1 => source = Some(items.into_iter().next().unwrap_or_default()),
                    _ => {
                        return Err(SQLError::Unsupported(format!(
                            "{keyword}: AS 'obj_file', 'link_symbol' bodies"
                        )));
                    }
                }
            }
            "window" => {
                return Err(SQLError::Unsupported(format!(
                    "{keyword}: WINDOW functions"
                )));
            }
            // Planner / execution hints without engine semantics:
            // COST, ROWS, PARALLEL, SECURITY, LEAKPROOF, SET, SUPPORT.
            _ => {}
        }
    }

    let body = match (source, stmt.sql_body.as_deref()) {
        (Some(src), None) => FunctionBody::Source(src),
        (None, Some(node)) => FunctionBody::Statements(compile_sql_standard_body(node)?),
        (Some(_), Some(_)) => {
            return Err(SQLError::Unsupported(format!(
                "{keyword}: both AS body and SQL-standard body"
            )));
        }
        (None, None) => {
            return Err(SQLError::Unsupported(format!(
                "{keyword}: no function body"
            )));
        }
    };
    if language.is_empty() {
        if matches!(body, FunctionBody::Statements(_)) {
            language = "sql".into();
        } else {
            return Err(SQLError::Unsupported(format!(
                "{keyword}: no language specified"
            )));
        }
    }

    Ok(CreateFunction {
        name,
        or_replace: stmt.replace,
        is_procedure: stmt.is_procedure,
        params,
        returns,
        language,
        body,
        volatility,
        strict,
    })
}

/// Compile a SQL-standard function body (`RETURN expr` or
/// `BEGIN ATOMIC stmt; ... END`) into plain statements.
fn compile_sql_standard_body(node: &Node) -> Result<Vec<Statement>> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("empty SQL function body".into()));
    };
    match inner {
        NodeEnum::ReturnStmt(ret) => {
            let value = ret
                .returnval
                .as_deref()
                .ok_or_else(|| SQLError::Internal("RETURN without a value".into()))?;
            Ok(vec![select_of_expr(compile_expr(value)?)])
        }
        NodeEnum::List(list) => {
            let mut out = Vec::with_capacity(list.items.len());
            for item in &list.items {
                let Some(item_inner) = item.node.as_ref() else {
                    continue;
                };
                match item_inner {
                    // BEGIN ATOMIC wraps each statement in a nested list.
                    NodeEnum::List(stmts) => {
                        for s in &stmts.items {
                            out.push(compile_stmt(s)?);
                        }
                    }
                    NodeEnum::ReturnStmt(ret) => {
                        let value = ret
                            .returnval
                            .as_deref()
                            .ok_or_else(|| SQLError::Internal("RETURN without a value".into()))?;
                        out.push(select_of_expr(compile_expr(value)?));
                    }
                    _ => out.push(compile_stmt(item)?),
                }
            }
            Ok(out)
        }
        other => Err(SQLError::Unsupported(format!(
            "SQL function body node {other:?}"
        ))),
    }
}

/// `SELECT <expr>` statement wrapping a single expression.
fn select_of_expr(expr: Expr) -> Statement {
    Statement::Select(Box::new(crate::ast::SelectStmt {
        projections: vec![crate::ast::Projection { expr, alias: None }],
        from: None,
        r#where: None,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        having: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        with: Vec::new(),
        set_op: None,
        distinct: false,
        distinct_on: Vec::new(),
    }))
}

fn compile_do(stmt: &pg_query::protobuf::DoStmt) -> Result<Statement> {
    let mut language = "plpgsql".to_string();
    let mut body: Option<String> = None;
    for arg in &stmt.args {
        let Some(NodeEnum::DefElem(elem)) = arg.node.as_ref() else {
            continue;
        };
        match elem.defname.to_ascii_lowercase().as_str() {
            "as" => body = def_elem_string(elem),
            "language" => {
                if let Some(lang) = def_elem_string(elem) {
                    language = lang.to_ascii_lowercase();
                }
            }
            _ => {}
        }
    }
    let body = body.ok_or_else(|| SQLError::Internal("DO without a body".into()))?;
    Ok(Statement::DoBlock { language, body })
}

fn compile_call(stmt: &pg_query::protobuf::CallStmt) -> Result<Statement> {
    let call = stmt
        .funccall
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CALL without a function".into()))?;
    let name = call
        .funcname
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(NodeEnum::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .next_back()
        .ok_or_else(|| SQLError::Internal("CALL without a procedure name".into()))?
        .to_ascii_lowercase();
    let args = call
        .args
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    Ok(Statement::Call { name, args })
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
    let returning = compile_projections(&stmt.returning_list)?;
    let with = match stmt.with_clause.as_ref() {
        Some(wc) => compile_with_clause(wc)?,
        None => Vec::new(),
    };
    Ok(UpdateStmt {
        table,
        assignments,
        r#where,
        with,
        from,
        returning,
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
    let returning = compile_projections(&stmt.returning_list)?;
    let with = match stmt.with_clause.as_ref() {
        Some(wc) => compile_with_clause(wc)?,
        None => Vec::new(),
    };
    Ok(DeleteStmt {
        table,
        r#where,
        with,
        using,
        returning,
    })
}

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
/// reduced to an arity because the engine resolves user-defined
/// routines by `(name, argument count)`.
fn compile_drop_function(
    stmt: &pg_query::protobuf::DropStmt,
    is_procedure: bool,
) -> Result<Statement> {
    use crate::ast::{DropFunctionItem, DropFunctionStmt};
    let mut items = Vec::new();
    for object in &stmt.objects {
        let Some(NodeEnum::ObjectWithArgs(owa)) = object.node.as_ref() else {
            return Err(SQLError::Unsupported(
                "DROP FUNCTION target is not a function signature".into(),
            ));
        };
        let name = owa
            .objname
            .iter()
            .filter_map(|n| match n.node.as_ref() {
                Some(NodeEnum::String(s)) => Some(s.sval.clone()),
                _ => None,
            })
            .next_back()
            .ok_or_else(|| SQLError::Internal("DROP FUNCTION without a name".into()))?;
        let arg_types = if owa.args_unspecified {
            None
        } else {
            Some(
                owa.objargs
                    .iter()
                    .map(|arg| match arg.node.as_ref() {
                        Some(NodeEnum::TypeName(t)) => Ok(compile_function_type_name(t)),
                        other => Err(SQLError::Unsupported(format!(
                            "DROP FUNCTION argument type node {other:?}"
                        ))),
                    })
                    .collect::<Result<Vec<_>>>()?,
            )
        };
        items.push(DropFunctionItem {
            name: name.to_ascii_lowercase(),
            arg_types,
        });
    }
    if items.is_empty() {
        return Err(SQLError::Internal("DROP FUNCTION without target".into()));
    }
    Ok(Statement::DropFunction(DropFunctionStmt {
        is_procedure,
        if_exists: stmt.missing_ok,
        items,
    }))
}

fn compile_drop(stmt: &pg_query::protobuf::DropStmt) -> Result<Statement> {
    use pg_query::protobuf::{DropBehavior, ObjectType};
    let kind = match stmt.remove_type() {
        ObjectType::ObjectTable => DropKind::Table,
        ObjectType::ObjectIndex => DropKind::Index,
        ObjectType::ObjectView => DropKind::View,
        ObjectType::ObjectSchema => DropKind::Schema,
        ObjectType::ObjectFunction => return compile_drop_function(stmt, false),
        ObjectType::ObjectProcedure => return compile_drop_function(stmt, true),
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
    Ok(Statement::Drop(DropStmt {
        kind,
        names,
        if_exists: stmt.missing_ok,
        cascade,
    }))
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
        AlterTableType::AtColumnDefault => {
            if let Some(default) = cmd.def.as_deref() {
                AlterTableAction::SetDefault {
                    name: cmd.name.clone(),
                    default: compile_expr(default)?,
                }
            } else {
                AlterTableAction::DropDefault {
                    name: cmd.name.clone(),
                }
            }
        }
        AlterTableType::AtSetNotNull => AlterTableAction::SetNotNull {
            name: cmd.name.clone(),
        },
        AlterTableType::AtDropNotNull => AlterTableAction::DropNotNull {
            name: cmd.name.clone(),
        },
        AlterTableType::AtAlterColumnType => {
            let def_inner = cmd
                .def
                .as_ref()
                .and_then(|d| d.node.as_ref())
                .ok_or_else(|| SQLError::Internal("ALTER COLUMN TYPE without type".into()))?;
            let ty = match def_inner {
                NodeEnum::ColumnDef(c) => compile_column_def(c)?.ty,
                NodeEnum::TypeName(t) => compile_pg_type_name(t, &cmd.name)?,
                other => {
                    return Err(SQLError::Internal(format!(
                        "ALTER COLUMN TYPE expected ColumnDef/TypeName, got {other:?}"
                    )));
                }
            };
            AlterTableAction::AlterColumnType {
                name: cmd.name.clone(),
                ty,
            }
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

pub fn plan_only_for_test(sql: &str) -> Result<Vec<Statement>> {
    compile(sql)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{ColumnType, FromClause};

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
    fn create_table_with_tensor_column() {
        let stmt = first("CREATE TABLE docs (id INTEGER PRIMARY KEY, chunks TENSOR(4))");
        let Statement::CreateTable(ct) = stmt else {
            panic!("not CREATE TABLE");
        };
        assert!(matches!(ct.columns[1].ty, ColumnType::Tensor(4)));
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
        match &s.r#where {
            Some(Expr::Func {
                distinct: false,
                order_by,
                filter: None,
                ..
            }) if order_by.is_empty() => {}
            other => panic!("expected scalar function call, got {other:?}"),
        }
        assert_eq!(s.order_by.len(), 1);
        assert!(s.order_by[0].descending);
        match &s.limit {
            Some(Expr::Literal(uqa_core::Value::Int(5))) => {}
            other => panic!("expected LIMIT 5, got {other:?}"),
        }
    }
}
