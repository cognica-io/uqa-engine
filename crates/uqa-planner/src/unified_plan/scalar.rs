//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL AST to executable scalar IR lowering and aggregate classification.

use super::{
    AggregateClassifier, Expr, FrameBound, OrderBy, QueryPlan, ScalarExpr, ScalarFrameBound,
    ScalarOrder, ScalarWindowFrame, ScalarWindowSpec, WindowSpec,
};

pub(super) fn lower_scalar_expression(
    expression: Expr,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> ScalarExpr {
    match expression {
        Expr::Star => ScalarExpr::Star,
        Expr::QualifiedStar(qualifier) => ScalarExpr::QualifiedStar(qualifier),
        Expr::Default => ScalarExpr::Default,
        Expr::Column(column) => ScalarExpr::Column(column),
        Expr::QualifiedColumn { qualifier, column } => {
            ScalarExpr::QualifiedColumn { qualifier, column }
        }
        Expr::Literal(value) => ScalarExpr::Literal(value),
        Expr::Param(index) => ScalarExpr::Param(index),
        Expr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => ScalarExpr::Func {
            name,
            binding,
            args: args
                .into_iter()
                .map(|argument| lower_scalar_expression(argument, aggregates, subqueries))
                .collect(),
            distinct,
            order_by: order_by
                .into_iter()
                .map(|order| lower_scalar_order(order, aggregates, subqueries))
                .collect(),
            filter: filter
                .map(|filter| Box::new(lower_scalar_expression(*filter, aggregates, subqueries))),
        },
        Expr::Array(items) => ScalarExpr::Array(
            items
                .into_iter()
                .map(|item| lower_scalar_expression(item, aggregates, subqueries))
                .collect(),
        ),
        Expr::Row(items) => ScalarExpr::Row(
            items
                .into_iter()
                .map(|item| lower_scalar_expression(item, aggregates, subqueries))
                .collect(),
        ),
        Expr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
            op,
            lhs: Box::new(lower_scalar_expression(*lhs, aggregates, subqueries)),
            rhs: Box::new(lower_scalar_expression(*rhs, aggregates, subqueries)),
        },
        Expr::UnaryMinus(expression) => ScalarExpr::UnaryMinus(Box::new(lower_scalar_expression(
            *expression,
            aggregates,
            subqueries,
        ))),
        Expr::Not(expression) => ScalarExpr::Not(Box::new(lower_scalar_expression(
            *expression,
            aggregates,
            subqueries,
        ))),
        Expr::And(items) => ScalarExpr::And(
            items
                .into_iter()
                .map(|item| lower_scalar_expression(item, aggregates, subqueries))
                .collect(),
        ),
        Expr::Or(items) => ScalarExpr::Or(
            items
                .into_iter()
                .map(|item| lower_scalar_expression(item, aggregates, subqueries))
                .collect(),
        ),
        Expr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(lower_scalar_expression(*expr, aggregates, subqueries)),
            negated,
        },
        Expr::Between { expr, low, high } => ScalarExpr::Between {
            expr: Box::new(lower_scalar_expression(*expr, aggregates, subqueries)),
            low: Box::new(lower_scalar_expression(*low, aggregates, subqueries)),
            high: Box::new(lower_scalar_expression(*high, aggregates, subqueries)),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(lower_scalar_expression(*expr, aggregates, subqueries)),
            list: list
                .into_iter()
                .map(|item| lower_scalar_expression(item, aggregates, subqueries))
                .collect(),
            negated,
        },
        Expr::WindowCall { name, args, spec } => ScalarExpr::WindowCall {
            name,
            args: args
                .into_iter()
                .map(|argument| lower_scalar_expression(argument, aggregates, subqueries))
                .collect(),
            spec: lower_scalar_window_spec(spec, aggregates, subqueries),
        },
        Expr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
            base: base.map(|base| Box::new(lower_scalar_expression(*base, aggregates, subqueries))),
            when: when
                .into_iter()
                .map(|(condition, result)| {
                    (
                        lower_scalar_expression(condition, aggregates, subqueries),
                        lower_scalar_expression(result, aggregates, subqueries),
                    )
                })
                .collect(),
            else_branch: else_branch
                .map(|branch| Box::new(lower_scalar_expression(*branch, aggregates, subqueries))),
        },
        Expr::Cast { expr, ty } => ScalarExpr::Cast {
            expr: Box::new(lower_scalar_expression(*expr, aggregates, subqueries)),
            ty,
        },
        Expr::ScalarSubquery(query) => {
            let id = subqueries.len();
            subqueries.push(QueryPlan::lower_with(*query, aggregates));
            ScalarExpr::ScalarSubquery(id)
        }
        Expr::Exists { body, negated } => {
            let id = subqueries.len();
            subqueries.push(QueryPlan::lower_with(*body, aggregates));
            ScalarExpr::Exists {
                subquery: id,
                negated,
            }
        }
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => {
            let expression = Box::new(lower_scalar_expression(*expr, aggregates, subqueries));
            let id = subqueries.len();
            subqueries.push(QueryPlan::lower_with(*body, aggregates));
            ScalarExpr::InSubquery {
                expr: expression,
                subquery: id,
                negated,
            }
        }
    }
}

pub(super) fn lower_scalar_order(
    order: OrderBy,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> ScalarOrder {
    ScalarOrder {
        expr: lower_scalar_expression(order.expr, aggregates, subqueries),
        descending: order.descending,
        nulls: order.nulls,
    }
}

pub(super) fn lower_scalar_window_spec(
    spec: WindowSpec,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> ScalarWindowSpec {
    assert!(
        spec.reference.is_none(),
        "named window reference must be resolved before unified-plan lowering"
    );
    ScalarWindowSpec {
        partition_by: spec
            .partition_by
            .into_iter()
            .map(|expression| lower_scalar_expression(expression, aggregates, subqueries))
            .collect(),
        order_by: spec
            .order_by
            .into_iter()
            .map(|order| lower_scalar_order(order, aggregates, subqueries))
            .collect(),
        frame: spec
            .frame
            .map(|frame| lower_scalar_window_frame(frame, aggregates, subqueries)),
    }
}

pub(super) fn lower_scalar_window_frame(
    frame: uqa_sql::ast::WindowFrame,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> ScalarWindowFrame {
    ScalarWindowFrame {
        mode: frame.mode,
        start: lower_scalar_frame_bound(frame.start, aggregates, subqueries),
        end: lower_scalar_frame_bound(frame.end, aggregates, subqueries),
    }
}

pub(super) fn lower_scalar_frame_bound(
    bound: FrameBound,
    aggregates: &dyn AggregateClassifier,
    subqueries: &mut Vec<QueryPlan>,
) -> ScalarFrameBound {
    match bound {
        FrameBound::UnboundedPreceding => ScalarFrameBound::UnboundedPreceding,
        FrameBound::UnboundedFollowing => ScalarFrameBound::UnboundedFollowing,
        FrameBound::CurrentRow => ScalarFrameBound::CurrentRow,
        FrameBound::Preceding(expression) => ScalarFrameBound::Preceding(Box::new(
            lower_scalar_expression(*expression, aggregates, subqueries),
        )),
        FrameBound::Following(expression) => ScalarFrameBound::Following(Box::new(
            lower_scalar_expression(*expression, aggregates, subqueries),
        )),
    }
}

pub(crate) fn is_builtin_aggregate(name: &str) -> bool {
    uqa_sql::ast::is_builtin_aggregate_function(name)
}
