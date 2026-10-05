//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recursive scalar and subquery reference validation.

use super::super::{SQLError, SQLParam, ScalarExpr};
use super::functions::{
    validate_qualified_column, validate_scalar_function, validate_unqualified_column,
    validate_window_function, ScalarFunctionValidation,
};
use crate::routines::RoutineResolution;
use crate::{FunctionTypeResolver, RowSchema, ScalarFrameBound};

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
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
            distinct,
            order_by,
            filter,
        } => {
            for (argument_index, argument) in args.iter().enumerate() {
                if crate::semantics::is_semantic_field_argument(name, args, argument_index)? {
                    continue;
                }
                validate_expression(routines, argument, schema, None, params, resolver)?;
            }
            for order in order_by {
                validate_expression(routines, &order.expr, schema, None, params, resolver)?;
            }
            if let Some(filter) = filter.as_deref() {
                validate_expression(routines, filter, schema, None, params, resolver)?;
                require_boolean_condition(filter, "FILTER", schema, params, resolver)?;
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
            )?;
            if crate::semantics::is_builtin_aggregate(expression)
                || routines.has_registered_aggregate_function(name)
            {
                return Ok(());
            }
            // `ParseFuncOrColumn` names the first aggregate modifier an ordinary function cannot take.
            let modifier = if *distinct {
                Some("DISTINCT")
            } else if !order_by.is_empty() {
                Some("ORDER BY")
            } else if filter.is_some() {
                Some("FILTER")
            } else {
                None
            };
            modifier.map_or(Ok(()), |modifier| {
                Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!(
                        "{modifier} specified, but {name} is not an aggregate function"
                    ),
                })
            })
        }
        ScalarExpr::WindowCall {
            name,
            args,
            spec,
            filter,
            modifiers,
        } => {
            for argument in args {
                validate_expression(routines, argument, schema, None, params, resolver)?;
            }
            if let Some(filter) = filter.as_deref() {
                validate_expression(routines, filter, schema, None, params, resolver)?;
                require_boolean_condition(filter, "FILTER", schema, params, resolver)?;
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
            validate_window_function(
                routines,
                name,
                (args, filter.is_some(), *modifiers),
                schema,
                params,
                resolver,
            )
        }
        ScalarExpr::Array(items) | ScalarExpr::Row(items) => {
            validate_items(routines, items, schema, params, resolver)
        }
        ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            let construct = if matches!(expression, ScalarExpr::And(_)) {
                "AND"
            } else {
                "OR"
            };
            for item in items {
                validate_expression(routines, item, schema, None, params, resolver)?;
                require_boolean_condition(item, construct, schema, params, resolver)?;
            }
            Ok(())
        }
        ScalarExpr::Not(inner) => {
            validate_expression(routines, inner, schema, None, params, resolver)?;
            require_boolean_condition(inner, "NOT", schema, params, resolver)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            validate_expression(routines, lhs, schema, None, params, resolver)?;
            validate_expression(routines, rhs, schema, None, params, resolver)
        }
        ScalarExpr::UnaryMinus(inner)
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
                // A searched CASE tests each condition; a simple CASE compares the operand with each value.
                if base.is_none() {
                    require_boolean_condition(condition, "CASE/WHEN", schema, params, resolver)?;
                }
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
        ScalarExpr::Star
        | ScalarExpr::Default
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::Param(_) => Ok(()),
    }
}

/// `coerce_to_boolean`: the condition of `construct` must be boolean. An `unknown` literal is read as boolean input, and the engine's retrieval predicates, which the planner turns into index searches, qualify as conditions.
pub(super) fn require_boolean_condition(
    condition: &ScalarExpr,
    construct: &str,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<(), SQLError> {
    if let ScalarExpr::Literal(uqa_core::Value::Str(text)) = condition {
        return crate::expr::parse_boolean_input(text)
            .map(drop)
            .ok_or_else(|| crate::expr::invalid_boolean_input(text));
    }
    if let ScalarExpr::Func { name, .. } = condition {
        if crate::registry::is_registered(&crate::semantics::builtin_function_dispatch_name(name)) {
            return Ok(());
        }
    }
    let ty = crate::scalar_type_with_resolver(condition, schema, params, resolver)?;
    let Some(ty) = crate::effective_overload_argument_type_with_params(condition, ty, params)
    else {
        return Ok(());
    };
    let mut base = &ty;
    while let crate::ast::ColumnType::Domain { base: inner, .. } = base {
        base = inner;
    }
    if matches!(base, crate::ast::ColumnType::Boolean) {
        Ok(())
    } else {
        Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: format!(
                "argument of {construct} must be type boolean, not type {}",
                ty.regtype_name()
            ),
        })
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
