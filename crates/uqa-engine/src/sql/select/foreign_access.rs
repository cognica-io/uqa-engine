//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-table pushdown and execution.

use super::{
    eval_scalar, execute_query_block_operator_output, expand_star_columns, projection_columns,
    BinaryOp, ComputePlan, CteScope, Engine, QueryBlockPlan, QueryOutput, QueryOutputMode,
    SQLError, SQLParam, ScalarEvalContext, ScalarExpr, Value,
};

#[allow(clippy::too_many_arguments)]
pub(in crate::sql) fn run_single_foreign_select_output(
    engine: &Engine,
    table: &str,
    block: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let predicates = fdw_predicates_from_where(stmt.r#where.as_ref(), params);
    let scanned = engine
        .scan_foreign_table_stream(table, None, &predicates, None)
        .map_err(SQLError::Unsupported)?;
    let columns = if matches!(block.compute, ComputePlan::Project) {
        expand_star_columns(
            projection_columns(&stmt.projections),
            &stmt.projections,
            engine,
            Some(table),
        )?
    } else {
        projection_columns(&stmt.projections)
    };
    let source_columns = engine
        .foreign_table_columns(table)
        .map_err(SQLError::Unsupported)?;
    let source: Box<dyn uqa_execution::PhysicalOperator + '_> =
        Box::new(uqa_execution::RowIteratorScan::new(
            source_columns,
            Box::new(scanned.map(|row| {
                row.map_err(SQLError::Unsupported)
                    .map_err(uqa_execution::ExecError::from)
            })),
        ));
    execute_query_block_operator_output(
        engine,
        source,
        stmt.r#where.clone(),
        stmt,
        block,
        params,
        ctes,
        columns,
        output_mode,
    )
}

pub(in crate::sql) fn fdw_predicates_from_where(
    expr: Option<&ScalarExpr>,
    params: &[SQLParam],
) -> Vec<uqa_fdw::FDWPredicate> {
    let Some(expr) = expr else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_fdw_predicates(expr, params, &mut out);
    out
}

pub(in crate::sql) fn collect_fdw_predicates(
    expr: &ScalarExpr,
    params: &[SQLParam],
    out: &mut Vec<uqa_fdw::FDWPredicate>,
) {
    match expr {
        ScalarExpr::And(parts) => {
            for part in parts {
                collect_fdw_predicates(part, params, out);
            }
        }
        _ => {
            if let Some(predicate) = fdw_predicate(expr, params) {
                out.push(predicate);
            }
        }
    }
}

pub(in crate::sql) fn fdw_predicate(
    expr: &ScalarExpr,
    params: &[SQLParam],
) -> Option<uqa_fdw::FDWPredicate> {
    match expr {
        ScalarExpr::Binary { op, lhs, rhs } => {
            if let Some(column) = fdw_column_name(lhs) {
                let value = fdw_const_value(rhs, params)?;
                return Some(uqa_fdw::FDWPredicate {
                    column,
                    operator: fdw_binary_op(*op)?,
                    value,
                });
            }
            if let Some(column) = fdw_column_name(rhs) {
                let value = fdw_const_value(lhs, params)?;
                return Some(uqa_fdw::FDWPredicate {
                    column,
                    operator: fdw_reversed_binary_op(*op)?,
                    value,
                });
            }
            None
        }
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } if !negated => {
            let column = fdw_column_name(expr)?;
            let values = list
                .iter()
                .map(|item| fdw_const_value(item, params))
                .collect::<Option<Vec<_>>>()?;
            Some(uqa_fdw::FDWPredicate {
                column,
                operator: uqa_fdw::PredicateOp::In,
                value: Value::List(values),
            })
        }
        ScalarExpr::IsNull { expr, negated } => Some(uqa_fdw::FDWPredicate {
            column: fdw_column_name(expr)?,
            operator: if *negated {
                uqa_fdw::PredicateOp::NotEq
            } else {
                uqa_fdw::PredicateOp::Eq
            },
            value: Value::Null,
        }),
        ScalarExpr::Func { name, args, .. } => fdw_like_predicate(name, args, false, params),
        ScalarExpr::Not(inner) => match inner.as_ref() {
            ScalarExpr::Func { name, args, .. } => fdw_like_predicate(name, args, true, params),
            _ => None,
        },
        _ => None,
    }
}

pub(in crate::sql) fn fdw_like_predicate(
    name: &str,
    args: &[ScalarExpr],
    negated: bool,
    params: &[SQLParam],
) -> Option<uqa_fdw::FDWPredicate> {
    if args.len() != 2 {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let operator = match (lower.as_str(), negated) {
        ("like", false) => uqa_fdw::PredicateOp::Like,
        ("like", true) => uqa_fdw::PredicateOp::NotLike,
        ("ilike", false) => uqa_fdw::PredicateOp::ILike,
        ("ilike", true) => uqa_fdw::PredicateOp::NotILike,
        _ => return None,
    };
    Some(uqa_fdw::FDWPredicate {
        column: fdw_column_name(&args[0])?,
        operator,
        value: fdw_const_value(&args[1], params)?,
    })
}

pub(in crate::sql) fn fdw_column_name(expr: &ScalarExpr) -> Option<String> {
    match expr {
        ScalarExpr::Column(name) => Some(name.clone()),
        ScalarExpr::QualifiedColumn { column, .. } => Some(column.clone()),
        _ => None,
    }
}

pub(in crate::sql) fn fdw_const_value(expr: &ScalarExpr, params: &[SQLParam]) -> Option<Value> {
    let ctx = ScalarEvalContext::new(None, params);
    eval_scalar(expr, &ctx).ok()
}

pub(in crate::sql) fn fdw_binary_op(op: BinaryOp) -> Option<uqa_fdw::PredicateOp> {
    Some(match op {
        BinaryOp::Equal => uqa_fdw::PredicateOp::Eq,
        BinaryOp::NotEqual => uqa_fdw::PredicateOp::NotEq,
        BinaryOp::Less => uqa_fdw::PredicateOp::Lt,
        BinaryOp::LessEqual => uqa_fdw::PredicateOp::LtEq,
        BinaryOp::Greater => uqa_fdw::PredicateOp::Gt,
        BinaryOp::GreaterEqual => uqa_fdw::PredicateOp::GtEq,
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => return None,
    })
}

pub(in crate::sql) fn fdw_reversed_binary_op(op: BinaryOp) -> Option<uqa_fdw::PredicateOp> {
    Some(match op {
        BinaryOp::Equal => uqa_fdw::PredicateOp::Eq,
        BinaryOp::NotEqual => uqa_fdw::PredicateOp::NotEq,
        BinaryOp::Less => uqa_fdw::PredicateOp::Gt,
        BinaryOp::LessEqual => uqa_fdw::PredicateOp::GtEq,
        BinaryOp::Greater => uqa_fdw::PredicateOp::Lt,
        BinaryOp::GreaterEqual => uqa_fdw::PredicateOp::LtEq,
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => return None,
    })
}
