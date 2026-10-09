//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent comparisons for parsed and retained range predicates.

use crate::{ast::BinaryOp, SQLError, ScalarExpr};

/// Construct comparisons in `PostgreSQL` analysis order. The boolean constructor's flag selects conjunction, and the comparison constructor's flag selects the upper written bound.
pub(crate) fn expand<T, E>(
    symmetric: bool,
    negated: bool,
    mut comparison: impl FnMut(BinaryOp, bool) -> Result<T, E>,
    boolean: impl Fn(bool, Vec<T>) -> T,
) -> Result<T, E> {
    let low = if negated {
        BinaryOp::Less
    } else {
        BinaryOp::GreaterEqual
    };
    let high = if negated {
        BinaryOp::Greater
    } else {
        BinaryOp::LessEqual
    };
    let forward = boolean(
        !negated,
        vec![comparison(low, false)?, comparison(high, true)?],
    );
    if symmetric {
        let backward = boolean(
            !negated,
            vec![comparison(low, true)?, comparison(high, false)?],
        );
        Ok(boolean(negated, vec![forward, backward]))
    } else {
        Ok(forward)
    }
}

/// Normalize already visited syntax without resolving names or changing stored bindings. Lowering each copied subtree subsequently allocates independent subquery slots.
pub(crate) fn restore_ast_node(node: &mut crate::ast::Expr) -> bool {
    use crate::ast::Expr;
    let (value, low, high, symmetric) = match &*node {
        Expr::Between { expr, low, high } => (expr.as_ref(), low.as_ref(), high.as_ref(), false),
        Expr::Func {
            binding: Some(binding),
            args,
            distinct,
            order_by,
            filter,
            ..
        } if binding.builtin
            && binding.dispatch == Some(crate::ast::FunctionDispatch::BetweenSymmetric)
            && binding.resolution_error.is_none()
            && !distinct
            && order_by.is_empty()
            && filter.is_none() =>
        {
            let [value, low, high] = args.as_slice() else {
                return false;
            };
            (value, low, high, true)
        }
        _ => return false,
    };
    *node = expand(
        symmetric,
        false,
        |op, upper| {
            Ok::<_, std::convert::Infallible>(Expr::Binary {
                op,
                lhs: Box::new(value.clone()),
                rhs: Box::new(if upper { high } else { low }.clone()),
            })
        },
        |and, items| {
            if and {
                Expr::And(items)
            } else {
                Expr::Or(items)
            }
        },
    )
    .unwrap_or_else(|never| match never {});
    true
}

/// Restore retained scalar plans and their independent nested query arenas without rebinding catalog identities.
pub(crate) fn restore_expression(
    expression: &mut crate::plan::ExpressionPlan,
) -> Result<bool, SQLError> {
    let mut changed = false;
    crate::plan::subqueries::rewrite_expression_with_arena(
        &mut expression.scalar,
        &mut expression.subqueries,
        &mut |node, arena| {
            changed |= restore_node(node, arena)?;
            Ok(())
        },
    )?;
    Ok(changed)
}

/// Retained plans predate the compiler expansion. Each copied operand keeps its original bindings and receives independent scalar-subquery initialization slots.
pub(crate) fn restore_query(query: &mut crate::plan::QueryPlan) -> Result<bool, SQLError> {
    let mut changed = false;
    crate::plan::subqueries::rewrite_query_with_arenas(query, &mut |node, arena| {
        changed |= restore_node(node, arena)?;
        Ok(())
    })?;
    Ok(changed)
}

fn restore_node(
    node: &mut ScalarExpr,
    arena: &mut Vec<crate::plan::QueryPlan>,
) -> Result<bool, SQLError> {
    let (value, low, high, symmetric) = match &*node {
        ScalarExpr::Between { expr, low, high } => {
            (expr.as_ref(), low.as_ref(), high.as_ref(), false)
        }
        ScalarExpr::Func {
            binding: Some(binding),
            args,
            distinct,
            order_by,
            filter,
            ..
        } if binding.builtin
            && binding.dispatch == Some(crate::ast::FunctionDispatch::BetweenSymmetric) =>
        {
            if let Some(error) = &binding.resolution_error {
                return Err(error.sql_error());
            }
            let [value, low, high] = args.as_slice() else {
                return Err(SQLError::Internal(
                    "stored symmetric range must have three operands".into(),
                ));
            };
            if *distinct || !order_by.is_empty() || filter.is_some() {
                return Err(SQLError::Internal(
                    "stored symmetric range has function modifiers".into(),
                ));
            }
            (value, low, high, true)
        }
        _ => return Ok(false),
    };
    let mut occurrence = 0;
    let expression = expand(
        symmetric,
        false,
        |op, upper| {
            let mut lhs = value.clone();
            let mut rhs = if upper { high } else { low }.clone();
            // Keep the first occurrences in their original slots; only repeated operands get new slots, so no unused query children remain.
            if occurrence > 0 {
                crate::plan::subqueries::copy_occurrences(&mut lhs, arena)?;
            }
            if occurrence >= 2 {
                crate::plan::subqueries::copy_occurrences(&mut rhs, arena)?;
            }
            occurrence += 1;
            Ok::<_, SQLError>(ScalarExpr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            })
        },
        |and, items| {
            if and {
                ScalarExpr::And(items)
            } else {
                ScalarExpr::Or(items)
            }
        },
    )?;
    *node = expression;
    Ok(true)
}

#[cfg(test)]
mod tests;
