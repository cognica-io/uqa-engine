//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT and ON CONFLICT lowering.

use super::{
    compile_expr, compile_returning_clause, compile_select, compile_with_clause, range_var_name,
    Expr, InsertStmt, NodeEnum, Result, SQLError,
};

pub(in crate::compiler) fn compile_insert(
    stmt: &pg_query::protobuf::InsertStmt,
) -> Result<InsertStmt> {
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("INSERT without relation".into()))?;
    let table = range_var_name(relation);
    let target_qualifier = relation
        .alias
        .as_ref()
        .map(|alias| alias.aliasname.as_str())
        .filter(|alias| !alias.is_empty())
        .unwrap_or(&relation.relname)
        .to_string();
    let columns = stmt
        .cols
        .iter()
        .map(|column| match column.node.as_ref() {
            Some(NodeEnum::ResTarget(target)) if !target.name.is_empty() => Ok(target.name.clone()),
            other => Err(SQLError::Internal(format!(
                "INSERT column target is malformed: {other:?}"
            ))),
        })
        .collect::<Result<Vec<_>>>()?;
    let mut rows = Vec::new();
    let select_source = if let Some(select_node) = stmt.select_stmt.as_ref() {
        let select_inner = select_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("INSERT select_stmt empty".into()))?;
        let select = match select_inner {
            NodeEnum::SelectStmt(s) => s,
            _ => return Err(SQLError::Unsupported("INSERT body must be SELECT".into())),
        };
        for row_node in &select.values_lists {
            let inner = row_node
                .node
                .as_ref()
                .ok_or_else(|| SQLError::Internal("INSERT VALUES contains an empty row".into()))?;
            let list = match inner {
                NodeEnum::List(l) => l,
                other => {
                    return Err(SQLError::Internal(format!(
                        "INSERT VALUES expected a row list, got {other:?}"
                    )));
                }
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
        if rows.is_empty()
            && (select.op != pg_query::protobuf::SetOperation::SetopNone as i32
                || !select.from_clause.is_empty()
                || !select.target_list.is_empty())
        {
            Some(Box::new(compile_select(select)?))
        } else {
            None
        }
    } else {
        // PostgreSQL represents DEFAULT VALUES by omitting select_stmt; one empty input row preserves its one-row cardinality while letting the ordinary default pipeline fill it.
        rows.push(Vec::new());
        None
    };
    let on_conflict = stmt
        .on_conflict_clause
        .as_ref()
        .map(|c| compile_on_conflict(c.as_ref()))
        .transpose()?;
    let (returning, returning_aliases) = compile_returning_clause(stmt.returning_clause.as_ref())?;
    let with = match stmt.with_clause.as_ref() {
        Some(wc) => compile_with_clause(wc)?,
        None => Vec::new(),
    };
    Ok(InsertStmt {
        table,
        target_relation_bound: false,
        target_qualifier,
        include_descendants: relation.inh,
        columns,
        with,
        rows,
        select_source,
        on_conflict,
        returning,
        returning_aliases,
    })
}

pub(in crate::compiler) fn compile_on_conflict(
    clause: &pg_query::protobuf::OnConflictClause,
) -> Result<crate::ast::OnConflict> {
    use crate::ast::{OnConflict, OnConflictAction};
    use pg_query::protobuf::OnConflictAction as PgAction;

    let conflict_columns = clause
        .infer
        .as_ref()
        .map(|infer| {
            infer
                .index_elems
                .iter()
                .map(|elem| match elem.node.as_ref() {
                    Some(NodeEnum::IndexElem(index)) if !index.name.is_empty() => {
                        Ok(index.name.clone())
                    }
                    other => Err(SQLError::Unsupported(format!(
                        "ON CONFLICT inference target {other:?}"
                    ))),
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();

    let action = match clause.action() {
        PgAction::OnconflictNothing => OnConflictAction::Nothing,
        PgAction::OnconflictUpdate => {
            let mut assignments: Vec<(String, Expr)> = Vec::new();
            for tgt in &clause.target_list {
                let inner = tgt.node.as_ref().ok_or_else(|| {
                    SQLError::Internal("ON CONFLICT UPDATE contains an empty assignment".into())
                })?;
                let NodeEnum::ResTarget(rt) = inner else {
                    return Err(SQLError::Internal(format!(
                        "ON CONFLICT UPDATE expected ResTarget, got {inner:?}"
                    )));
                };
                let val = rt.val.as_ref().ok_or_else(|| {
                    SQLError::Internal("ON CONFLICT UPDATE assignment has no value".into())
                })?;
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
                r#where: where_clause.map(Box::new),
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
