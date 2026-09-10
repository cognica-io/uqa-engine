//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! WITH body compilation and recursive query validation.

use super::{
    compile_expr, compile_select, extract_strings, Expr, FromClause, NodeEnum, Result, SQLError,
    SelectStmt, Value, CTE,
};
use crate::ast::{CteCycleClause, CteMaterialization, CteSearchClause};

#[expect(clippy::too_many_lines, reason = "preserves PostgreSQL lowering order")]
pub(in crate::compiler) fn compile_with_clause(
    wc: &pg_query::protobuf::WithClause,
) -> Result<Vec<CTE>> {
    let mut out = Vec::with_capacity(wc.ctes.len());
    for cte_node in &wc.ctes {
        let inner = cte_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("WITH contains an empty CTE".into()))?;
        let cte = match inner {
            NodeEnum::CommonTableExpr(c) => c,
            _ => return Err(SQLError::Internal("expected CommonTableExpr".into())),
        };
        if cte.ctename.is_empty() {
            return Err(SQLError::Internal("CTE name is empty".into()));
        }
        let materialization = match cte.ctematerialized() {
            pg_query::protobuf::CteMaterialize::CtematerializeUndefined
            | pg_query::protobuf::CteMaterialize::Default => CteMaterialization::Default,
            pg_query::protobuf::CteMaterialize::Always => CteMaterialization::Materialized,
            pg_query::protobuf::CteMaterialize::Never => CteMaterialization::NotMaterialized,
        };
        let select_node = cte
            .ctequery
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CTE without query".into()))?;
        let select_inner = select_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CTE query node empty".into()))?;
        let body = match select_inner {
            NodeEnum::SelectStmt(select) => {
                crate::ast::CteBody::Query(Box::new(compile_select(select)?))
            }
            _ => crate::ast::CteBody::try_from(crate::compiler::dispatch::compile_stmt(
                select_node,
            )?)?,
        };
        let columns = extract_strings(&cte.aliascolnames)?;
        let search = cte
            .search_clause
            .as_ref()
            .map(|clause| -> Result<CteSearchClause> {
                Ok(CteSearchClause {
                    columns: extract_strings(&clause.search_col_list)?,
                    breadth_first: clause.search_breadth_first,
                    sequence_column: clause.search_seq_column.clone(),
                })
            })
            .transpose()?;
        let cycle = cte
            .cycle_clause
            .as_deref()
            .map(|clause| -> Result<CteCycleClause> {
                let mark_value = clause
                    .cycle_mark_value
                    .as_deref()
                    .map(compile_expr)
                    .transpose()?
                    .unwrap_or(Expr::Literal(Value::Bool(true)));
                let mark_default = clause
                    .cycle_mark_default
                    .as_deref()
                    .map(compile_expr)
                    .transpose()?
                    .unwrap_or(Expr::Literal(Value::Bool(false)));
                Ok(CteCycleClause {
                    columns: extract_strings(&clause.cycle_col_list)?,
                    mark_column: clause.cycle_mark_column.clone(),
                    mark_value,
                    mark_default,
                    path_column: clause.cycle_path_column.clone(),
                })
            })
            .transpose()?;
        let recursive = wc.recursive
            && body
                .query()
                .is_some_and(|query| select_references_relation(query, &cte.ctename));
        if let Some(query) = body.query().filter(|_| recursive) {
            reject_recursive_query_ordering(query)?;
        }
        if (search.is_some() || cycle.is_some()) && !recursive {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "WITH query is not recursive".into(),
            });
        }
        if let (Some(search), Some(cycle)) = (&search, &cycle) {
            if search.sequence_column == cycle.mark_column {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: "search sequence column name and cycle mark column name are the same"
                        .into(),
                });
            }
            if search.sequence_column == cycle.path_column {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: "search sequence column name and cycle path column name are the same"
                        .into(),
                });
            }
        }
        if cycle
            .as_ref()
            .is_some_and(|cycle| cycle.mark_column == cycle.path_column)
        {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "cycle mark column name and cycle path column name are the same".into(),
            });
        }
        out.push(CTE {
            name: cte.ctename.clone(),
            columns,
            recursive: wc.recursive,
            materialization,
            search,
            cycle,
            body,
        });
    }
    Ok(out)
}

fn select_references_relation(statement: &SelectStmt, name: &str) -> bool {
    fn from_references_relation(from: &FromClause, name: &str) -> bool {
        match from {
            FromClause::Table { name: table, .. } => table == name,
            FromClause::Join { left, right, .. } => {
                from_references_relation(left, name) || from_references_relation(right, name)
            }
            FromClause::Subquery { body, .. } => select_references_relation(body, name),
            FromClause::Values { .. }
            | FromClause::Function { .. }
            | FromClause::FunctionGroup { .. } => false,
        }
    }
    statement
        .from
        .as_ref()
        .is_some_and(|from| from_references_relation(from, name))
        || statement.set_op.as_ref().is_some_and(|set| {
            set.left
                .as_deref()
                .is_some_and(|left| select_references_relation(left, name))
                || select_references_relation(&set.right, name)
        })
}

fn reject_recursive_query_ordering(statement: &SelectStmt) -> Result<()> {
    let Some(set_op) = statement.set_op.as_ref() else {
        return Ok(());
    };
    if !set_op.combined_order_by.is_empty() {
        return Err(SQLError::Unsupported(
            "ORDER BY in a recursive query is not implemented".into(),
        ));
    }
    if set_op.combined_offset.is_some() {
        return Err(SQLError::Unsupported(
            "OFFSET in a recursive query is not implemented".into(),
        ));
    }
    if set_op.combined_limit.is_some() {
        return Err(SQLError::Unsupported(
            "LIMIT in a recursive query is not implemented".into(),
        ));
    }
    Ok(())
}
