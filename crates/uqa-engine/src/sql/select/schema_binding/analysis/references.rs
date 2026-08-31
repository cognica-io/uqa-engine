//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recursive scalar and subquery reference validation.

use super::super::{SQLError, SQLParam, ScalarExpr};
use super::functions::{
    is_semantic_all_argument, validate_qualified_column, validate_scalar_function,
    validate_unqualified_column, validate_window_function, ScalarFunctionValidation,
};
use crate::engine_user_functions::RoutineResolution;
use uqa_execution::{FunctionTypeResolver, RowSchema, ScalarFrameBound};

pub(super) fn validate_expression(
    routines: &dyn RoutineResolution,
    expression: &ScalarExpr,
    schema: &RowSchema,
    fallback: Option<&RowSchema>,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
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
        ScalarExpr::InternalColumn(column) => {
            if schema.internal_slot(*column).is_some()
                || fallback.is_some_and(|fallback| fallback.internal_slot(*column).is_some())
            {
                Ok(())
            } else {
                Err(SQLError::Internal(format!(
                    "internal relation attribute {column:?} is outside the bound row scope"
                )))
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
                validate_expression(routines, argument, schema, None, params, resolver)?;
            }
            for order in order_by {
                validate_expression(routines, &order.expr, schema, None, params, resolver)?;
            }
            if let Some(filter) = filter.as_deref() {
                validate_expression(routines, filter, schema, None, params, resolver)?;
            }
            validate_scalar_function(
                routines,
                ScalarFunctionValidation {
                    name,
                    binding: binding.as_ref(),
                    args,
                    order_by,
                    expression,
                    schema,
                    params,
                    resolver,
                },
            )
        }
        ScalarExpr::WindowCall { name, args, spec } => {
            for argument in args {
                validate_expression(routines, argument, schema, None, params, resolver)?;
            }
            for expression in &spec.partition_by {
                validate_expression(routines, expression, schema, None, params, resolver)?;
            }
            for order in &spec.order_by {
                validate_expression(routines, &order.expr, schema, None, params, resolver)?;
            }
            if let Some(frame) = &spec.frame {
                for bound in [&frame.start, &frame.end] {
                    if let ScalarFrameBound::Preceding(expression)
                    | ScalarFrameBound::Following(expression) = bound
                    {
                        validate_expression(routines, expression, schema, None, params, resolver)?;
                    }
                }
            }
            validate_window_function(routines, name, args, schema, params, resolver)
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => validate_items(routines, items, schema, params, resolver),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            validate_expression(routines, lhs, schema, None, params, resolver)?;
            validate_expression(routines, rhs, schema, None, params, resolver)
        }
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            validate_expression(routines, inner, schema, None, params, resolver)
        }
        ScalarExpr::Between { expr, low, high } => {
            for item in [expr.as_ref(), low.as_ref(), high.as_ref()] {
                validate_expression(routines, item, schema, None, params, resolver)?;
            }
            Ok(())
        }
        ScalarExpr::InList { expr, list, .. } => {
            validate_expression(routines, expr, schema, None, params, resolver)?;
            validate_items(routines, list, schema, params, resolver)
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_deref() {
                validate_expression(routines, base, schema, None, params, resolver)?;
            }
            for (condition, value) in when {
                validate_expression(routines, condition, schema, None, params, resolver)?;
                validate_expression(routines, value, schema, None, params, resolver)?;
            }
            if let Some(else_branch) = else_branch.as_deref() {
                validate_expression(routines, else_branch, schema, None, params, resolver)?;
            }
            Ok(())
        }
        ScalarExpr::ScalarSubquery(index)
        | ScalarExpr::Exists {
            subquery: index, ..
        } => resolver
            .resolve_scalar_subquery_type(*index, schema, params)
            .map(drop),
        ScalarExpr::InSubquery { expr, subquery, .. } => {
            validate_expression(routines, expr, schema, None, params, resolver)?;
            resolver
                .resolve_scalar_subquery_type(*subquery, schema, params)
                .map(drop)
        }
        ScalarExpr::Star | ScalarExpr::Default | ScalarExpr::Literal(_) | ScalarExpr::Param(_) => {
            Ok(())
        }
    }
}

fn validate_items(
    routines: &dyn RoutineResolution,
    items: &[ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<(), SQLError> {
    for item in items {
        validate_expression(routines, item, schema, None, params, resolver)?;
    }
    Ok(())
}
