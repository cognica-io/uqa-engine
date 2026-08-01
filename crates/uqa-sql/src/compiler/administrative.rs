//! Session control, ANALYZE, EXPLAIN, TRUNCATE, and transactions.

use super::dispatch::compile_stmt;
use super::{
    range_var_name, render_relation_component, NodeEnum, Result, SQLError, Statement,
    TransactionStmt,
};

pub(super) fn discard_target(mode: i32) -> Result<crate::ast::DiscardTarget> {
    use crate::ast::DiscardTarget;
    match mode {
        1 => Ok(DiscardTarget::All),
        2 => Ok(DiscardTarget::Plans),
        3 => Ok(DiscardTarget::Sequences),
        4 => Ok(DiscardTarget::Temp),
        other => Err(SQLError::Internal(format!(
            "unknown DISCARD target {other}"
        ))),
    }
}

pub(super) fn compile_analyze(stmt: &pg_query::protobuf::VacuumStmt) -> Result<Statement> {
    if stmt.is_vacuumcmd {
        return Err(SQLError::Unsupported(
            "VACUUM is not implemented; VACUUM must not be treated as ANALYZE".into(),
        ));
    }

    if !stmt.options.is_empty() {
        return Err(SQLError::Unsupported(
            "ANALYZE options are not implemented".into(),
        ));
    }

    let table = match stmt.rels.as_slice() {
        [] => None,
        [node] => {
            let Some(NodeEnum::VacuumRelation(relation)) = node.node.as_ref() else {
                return Err(SQLError::Internal(
                    "ANALYZE contains a malformed relation".into(),
                ));
            };
            if relation.oid != 0 {
                return Err(SQLError::Unsupported(
                    "OID-targeted ANALYZE is not implemented".into(),
                ));
            }
            if !relation.va_cols.is_empty() {
                return Err(SQLError::Unsupported(
                    "ANALYZE column lists are not implemented".into(),
                ));
            }
            let range = relation.relation.as_ref().ok_or_else(|| {
                SQLError::Internal("ANALYZE relation is missing its table name".into())
            })?;
            if !range.catalogname.is_empty() {
                return Err(SQLError::Unsupported(
                    "cross-database ANALYZE is not implemented".into(),
                ));
            }
            if range.relname.is_empty() {
                return Err(SQLError::Internal(
                    "ANALYZE relation has an empty table name".into(),
                ));
            }
            Some(range_var_name(range))
        }
        _ => {
            return Err(SQLError::Unsupported(
                "ANALYZE of multiple tables is not implemented".into(),
            ));
        }
    };

    Ok(Statement::Analyze { table })
}

pub(super) fn compile_variable_set(
    stmt: &pg_query::protobuf::VariableSetStmt,
) -> Result<Statement> {
    // Capture each argument as a string and join with commas. PG's
    // SET search_path TO a, b, c arrives as a list of A_Const nodes.
    let mut parts: Vec<String> = Vec::new();
    for arg in &stmt.args {
        let node = arg
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("SET contains an empty argument".into()))?;
        match node {
            NodeEnum::AConst(constant) => match constant.val.as_ref() {
                Some(pg_query::protobuf::a_const::Val::Sval(value)) => {
                    parts.push(value.sval.clone());
                }
                Some(pg_query::protobuf::a_const::Val::Ival(value)) => {
                    parts.push(value.ival.to_string());
                }
                Some(pg_query::protobuf::a_const::Val::Fval(value)) => {
                    parts.push(value.fval.clone());
                }
                Some(pg_query::protobuf::a_const::Val::Boolval(value)) => {
                    parts.push(value.boolval.to_string());
                }
                None if constant.isnull => parts.push("NULL".into()),
                other => {
                    return Err(SQLError::Unsupported(format!(
                        "SET argument {other:?} is not supported"
                    )));
                }
            },
            NodeEnum::TypeCast(cast) => {
                let Some(NodeEnum::AConst(constant)) = cast
                    .arg
                    .as_ref()
                    .and_then(|argument| argument.node.as_ref())
                else {
                    return Err(SQLError::Unsupported(
                        "SET type-cast argument must contain a literal".into(),
                    ));
                };
                let Some(pg_query::protobuf::a_const::Val::Sval(value)) = constant.val.as_ref()
                else {
                    return Err(SQLError::Unsupported(
                        "SET type-cast argument must contain a string literal".into(),
                    ));
                };
                parts.push(value.sval.clone());
            }
            NodeEnum::String(value) => parts.push(value.sval.clone()),
            other => {
                return Err(SQLError::Unsupported(format!(
                    "SET argument {other:?} is not supported"
                )));
            }
        }
    }
    let value = if stmt.name.eq_ignore_ascii_case("search_path") {
        parts
            .iter()
            .map(|part| render_relation_component(part))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        parts.join(",")
    };
    Ok(Statement::SetVariable {
        name: stmt.name.clone(),
        value,
    })
}

pub(super) fn compile_explain(stmt: &pg_query::protobuf::ExplainStmt) -> Result<Statement> {
    let body = stmt
        .query
        .as_deref()
        .ok_or_else(|| SQLError::Internal("EXPLAIN without body".into()))?;
    let mut analyze = false;
    let mut verbose = false;
    let mut format: Option<String> = None;
    for opt in &stmt.options {
        let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() else {
            return Err(SQLError::Internal(
                "EXPLAIN contains a malformed option".into(),
            ));
        };
        let name = elem.defname.to_ascii_lowercase();
        match name.as_str() {
            "analyze" => analyze = compile_explain_bool_option(elem, "ANALYZE")?,
            "verbose" => verbose = compile_explain_bool_option(elem, "VERBOSE")?,
            "format" => {
                if let Some(NodeEnum::String(s)) = elem.arg.as_ref().and_then(|a| a.node.as_ref()) {
                    format = Some(s.sval.clone());
                } else {
                    return Err(SQLError::TypeMismatch(
                        "EXPLAIN FORMAT expects a format name".into(),
                    ));
                }
            }
            _ => {
                return Err(SQLError::Unsupported(format!(
                    "EXPLAIN option `{name}` is not supported"
                )));
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

pub(super) fn compile_explain_bool_option(
    elem: &pg_query::protobuf::DefElem,
    name: &str,
) -> Result<bool> {
    let Some(argument) = elem
        .arg
        .as_ref()
        .and_then(|argument| argument.node.as_ref())
    else {
        return Ok(true);
    };
    match argument {
        NodeEnum::Boolean(value) => Ok(value.boolval),
        NodeEnum::Integer(value) => Ok(value.ival != 0),
        NodeEnum::String(value) => match value.sval.to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Ok(true),
            "false" | "off" | "no" | "0" => Ok(false),
            _ => Err(SQLError::TypeMismatch(format!(
                "EXPLAIN {name} expects a boolean value"
            ))),
        },
        _ => Err(SQLError::TypeMismatch(format!(
            "EXPLAIN {name} expects a boolean value"
        ))),
    }
}

pub(super) fn compile_truncate(stmt: &pg_query::protobuf::TruncateStmt) -> Result<Statement> {
    let mut tables = Vec::new();
    for relation in &stmt.relations {
        let Some(NodeEnum::RangeVar(range)) = relation.node.as_ref() else {
            return Err(SQLError::Internal(
                "TRUNCATE contains a malformed table target".into(),
            ));
        };
        tables.push(range_var_name(range));
    }
    if tables.is_empty() {
        return Err(SQLError::Internal("TRUNCATE without a table".into()));
    }
    let cascade = matches!(
        stmt.behavior(),
        pg_query::protobuf::DropBehavior::DropCascade
    );
    Ok(Statement::Truncate { tables, cascade })
}

pub(super) fn compile_transaction(stmt: &pg_query::protobuf::TransactionStmt) -> Result<Statement> {
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
