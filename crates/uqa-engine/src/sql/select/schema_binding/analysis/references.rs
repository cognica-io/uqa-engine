//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recursive scalar and subquery reference validation.

use super::super::{Engine, QueryPlan, SQLError, SQLParam, ScalarExpr, SchemaScope};
use super::functions::{
    is_semantic_all_argument, validate_qualified_column, validate_scalar_function,
    validate_unqualified_column, validate_window_function, ScalarFunctionValidation,
};
use uqa_execution::{RowSchema, ScalarFrameBound};

pub(super) fn validate_expression(
    scope: &mut SchemaScope,
    engine: &Engine,
    expression: &ScalarExpr,
    schema: &RowSchema,
    fallback: Option<&RowSchema>,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
) -> Result<(), SQLError> {
    match expression {
        ScalarExpr::Column(column) => validate_unqualified_column(schema, fallback, column),
        ScalarExpr::Position(position) => {
            if *position < schema.len() {
                Ok(())
            } else {
                Err(SQLError::UnknownColumn((position + 1).to_string()))
            }
        }
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            validate_qualified_column(schema, fallback, qualifier, column)
        }
        ScalarExpr::QualifiedStar(qualifier) => {
            if schema.has_qualifier(qualifier)
                || fallback.is_some_and(|fallback| fallback.has_qualifier(qualifier))
            {
                Ok(())
            } else {
                Err(SQLError::UnknownTable(qualifier.clone()))
            }
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                if is_semantic_all_argument(name, argument) {
                    continue;
                }
                validate_expression(scope, engine, argument, schema, None, subqueries, params)?;
            }
            for order in order_by {
                validate_expression(scope, engine, &order.expr, schema, None, subqueries, params)?;
            }
            if let Some(filter) = filter.as_deref() {
                validate_expression(scope, engine, filter, schema, None, subqueries, params)?;
            }
            validate_scalar_function(
                engine,
                ScalarFunctionValidation {
                    name,
                    binding: binding.as_ref(),
                    args,
                    order_by,
                    expression,
                    schema,
                    params,
                },
            )
        }
        ScalarExpr::WindowCall { name, args, spec } => {
            for argument in args {
                validate_expression(scope, engine, argument, schema, None, subqueries, params)?;
            }
            for expression in &spec.partition_by {
                validate_expression(scope, engine, expression, schema, None, subqueries, params)?;
            }
            for order in &spec.order_by {
                validate_expression(scope, engine, &order.expr, schema, None, subqueries, params)?;
            }
            if let Some(frame) = &spec.frame {
                for bound in [&frame.start, &frame.end] {
                    if let ScalarFrameBound::Preceding(expression)
                    | ScalarFrameBound::Following(expression) = bound
                    {
                        validate_expression(
                            scope, engine, expression, schema, None, subqueries, params,
                        )?;
                    }
                }
            }
            validate_window_function(engine, name, args, schema, params)
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => validate_items(scope, engine, items, schema, subqueries, params),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            validate_expression(scope, engine, lhs, schema, None, subqueries, params)?;
            validate_expression(scope, engine, rhs, schema, None, subqueries, params)
        }
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            validate_expression(scope, engine, inner, schema, None, subqueries, params)
        }
        ScalarExpr::Between { expr, low, high } => {
            for item in [expr.as_ref(), low.as_ref(), high.as_ref()] {
                validate_expression(scope, engine, item, schema, None, subqueries, params)?;
            }
            Ok(())
        }
        ScalarExpr::InList { expr, list, .. } => {
            validate_expression(scope, engine, expr, schema, None, subqueries, params)?;
            validate_items(scope, engine, list, schema, subqueries, params)
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_deref() {
                validate_expression(scope, engine, base, schema, None, subqueries, params)?;
            }
            for (condition, value) in when {
                validate_expression(scope, engine, condition, schema, None, subqueries, params)?;
                validate_expression(scope, engine, value, schema, None, subqueries, params)?;
            }
            if let Some(else_branch) = else_branch.as_deref() {
                validate_expression(scope, engine, else_branch, schema, None, subqueries, params)?;
            }
            Ok(())
        }
        ScalarExpr::ScalarSubquery(index)
        | ScalarExpr::Exists {
            subquery: index, ..
        } => validate_subquery(scope, engine, *index, schema, subqueries, params),
        ScalarExpr::InSubquery { expr, subquery, .. } => {
            validate_expression(scope, engine, expr, schema, None, subqueries, params)?;
            validate_subquery(scope, engine, *subquery, schema, subqueries, params)
        }
        ScalarExpr::Star | ScalarExpr::Default | ScalarExpr::Literal(_) | ScalarExpr::Param(_) => {
            Ok(())
        }
    }
}

fn validate_items(
    scope: &mut SchemaScope,
    engine: &Engine,
    items: &[ScalarExpr],
    schema: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for item in items {
        validate_expression(scope, engine, item, schema, None, subqueries, params)?;
    }
    Ok(())
}

fn validate_subquery(
    scope: &mut SchemaScope,
    engine: &Engine,
    index: usize,
    outer: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let plan = subqueries.get(index).ok_or_else(|| {
        SQLError::Internal(format!("scalar subquery slot {index} is out of bounds"))
    })?;
    scope
        .bind_query(engine, plan, params, Some(outer))
        .map(drop)
}
