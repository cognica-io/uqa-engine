//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL predicates eligible for foreign-handler pushdown.

use crate::{eval_scalar, ScalarEvalContext};
use uqa_core::Value;
use uqa_sql::{ast::BinaryOp, SQLParam, ScalarExpr};

pub fn fdw_predicates_from_where(
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

pub fn collect_fdw_predicates(
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

pub fn fdw_predicate(expr: &ScalarExpr, params: &[SQLParam]) -> Option<uqa_fdw::FDWPredicate> {
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
        ScalarExpr::Func { binding, args, .. } => {
            fdw_like_predicate(binding.as_ref(), args, false, params)
        }
        ScalarExpr::Not(inner) => match inner.as_ref() {
            ScalarExpr::Func { binding, args, .. } => {
                fdw_like_predicate(binding.as_ref(), args, true, params)
            }
            _ => None,
        },
        _ => None,
    }
}

/// A LIKE or NOT LIKE call with a determinate input collation. LIKE compares characters exactly under every built-in collation, so a handler keeps the rows the canonical operator keeps. ILIKE stays in the canonical evaluator: it lowercases through the provider of its input collation, while a handler folds case by its own rules, and a row a handler rejects is never evaluated again. The canonical operator likewise reports an indeterminate collation for each row it evaluates.
pub fn fdw_like_predicate(
    binding: Option<&uqa_sql::ast::FunctionBinding>,
    args: &[ScalarExpr],
    negated: bool,
    params: &[SQLParam],
) -> Option<uqa_fdw::FDWPredicate> {
    if args.len() != 2 {
        return None;
    }
    let uqa_sql::ast::FunctionDispatch::Pattern { operator, .. } = binding?.dispatch? else {
        return None;
    };
    if !operator.is_like()
        || operator.case_insensitive()
        || uqa_sql::ast::FunctionBinding::selected_input_collation(binding) == 0
    {
        return None;
    }
    let operator = if operator.negated() == negated {
        uqa_fdw::PredicateOp::Like
    } else {
        uqa_fdw::PredicateOp::NotLike
    };
    let value = fdw_const_value(&args[1], params)?;
    let Value::Str(pattern) = &value else {
        return None;
    };
    // Foreign SQL dialects do not agree on the implicit LIKE escape.
    // Keep backslash-bearing patterns in the canonical PostgreSQL evaluator.
    if pattern.contains('\\') {
        return None;
    }
    Some(uqa_fdw::FDWPredicate {
        column: fdw_column_name(&args[0])?,
        operator,
        value,
    })
}

pub fn fdw_column_name(expr: &ScalarExpr) -> Option<String> {
    match expr {
        ScalarExpr::Column(name) => Some(name.clone()),
        ScalarExpr::QualifiedColumn { column, .. } => Some(column.clone()),
        _ => None,
    }
}

pub fn fdw_const_value(expr: &ScalarExpr, params: &[SQLParam]) -> Option<Value> {
    let ctx = ScalarEvalContext::new(None, params);
    eval_scalar(expr, &ctx).ok()
}

pub fn fdw_binary_op(op: BinaryOp) -> Option<uqa_fdw::PredicateOp> {
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

pub fn fdw_reversed_binary_op(op: BinaryOp) -> Option<uqa_fdw::PredicateOp> {
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

#[cfg(test)]
mod tests;
