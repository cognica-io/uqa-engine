//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and retained callers share scalar inference while every synthesized type keeps its constructor lease.

use super::{
    cast_compatibility, common, functions, operators, qualified_column, FunctionTypeResolver,
};
use crate::{ast::ColumnType, schema::ScalarTypeSchema, SQLError, SQLParam, ScalarExpr};
use uqa_core::memory::{Produced, ProductionControl};

#[expect(
    clippy::too_many_lines,
    reason = "type resolution preserves candidate order and ambiguity diagnostics atomically"
)]
pub(super) fn scalar_type_inner_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    assert!(
        control.budget().is_none() || resolver.is_none(),
        "controlled type inference cannot invoke an unowned catalog resolver"
    );
    control.check()?;
    if matches!(
        expression,
        ScalarExpr::Func { binding, .. }
            if binding.as_ref().and_then(|binding| binding.dispatch).is_some_and(
                crate::ast::FunctionDispatch::is_call_argument_marker
            )
    ) {
        let argument = crate::scalar_call_argument(expression)?;
        return scalar_type_inner_with_control(argument.value, schema, params, resolver, control);
    }
    match expression {
        ScalarExpr::Column(column) => {
            if schema.has_unqualified_column(column) || schema.column_is_ambiguous(column) {
                copy_type(schema.type_of(column), control)
            } else if schema.has_qualifier(column) {
                copy_type(Some(&ColumnType::Record), control)
            } else {
                Ok(None)
            }
        }
        ScalarExpr::Position(position) => copy_type(schema.column_type(*position), control),
        ScalarExpr::InternalColumn(column) => copy_type(schema.internal_type(*column), control),
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            qualified_column::resolve_with_control(schema, qualifier, column, control)
        }
        ScalarExpr::Literal(value) => common::value_type_with_control(value, control),
        ScalarExpr::TypedLiteral {
            bound_type: Some(ty),
            ..
        } => copy_type(Some(ty), control),
        ScalarExpr::TypedLiteral { ty, .. } => {
            let target = match ColumnType::from_sql_name_with_control(ty, control) {
                Ok(ty) => Ok(Some(ty)),
                Err(error @ SQLError::Unsupported(_)) => match resolver {
                    Some(resolver) => resolver
                        .resolve_type_name(ty)?
                        .map_or(Err(error), |ty| Ok(Some(control.finish(ty, None)?))),
                    None => Err(error),
                },
                Err(error) => Err(error),
            }?;
            Ok(target)
        }
        ScalarExpr::Param(index) => index
            .checked_sub(1)
            .and_then(|index| params.get(index))
            .map(|parameter| common::parameter_type_with_control(parameter, control))
            .transpose()
            .map(Option::flatten),
        ScalarExpr::Cast { expr, ty } => {
            let source = scalar_type_inner_with_control(expr, schema, params, resolver, control)?;
            let target = match ColumnType::from_sql_name_with_control(ty, control) {
                Ok(ty) => Ok(Some(ty)),
                Err(error @ SQLError::Unsupported(_)) => match resolver {
                    Some(resolver) => resolver
                        .resolve_type_name(ty)?
                        .map_or(Err(error), |ty| Ok(Some(control.finish(ty, None)?))),
                    None => Err(error),
                },
                Err(error) => Err(error),
            }?;
            if let Some(target) = target.as_ref() {
                cast_compatibility::validate_explicit_cast_with_control(
                    source.as_deref(),
                    target,
                    control,
                )?;
            }
            Ok(target)
        }
        ScalarExpr::Array(items) => {
            if items.is_empty() {
                return Ok(None);
            }
            let mut element = None;
            for item in items {
                element = common::merge_value_types(
                    element,
                    common::common_context_expression_type_with_control(
                        item, schema, params, resolver, control,
                    )?,
                    control,
                )?;
            }
            let element =
                element.map_or_else(|| ColumnType::Text.clone_with_control(control), Ok)?;
            Ok(Some(ColumnType::array_with_control(element, control)?))
        }
        ScalarExpr::Row(items) => {
            for item in items {
                scalar_type_inner_with_control(item, schema, params, resolver, control)?;
            }
            copy_type(Some(&ColumnType::Record), control)
        }
        ScalarExpr::Binary { op, lhs, rhs } => {
            let left = common::common_context_expression_type_with_control(
                lhs, schema, params, resolver, control,
            )?;
            let right = common::common_context_expression_type_with_control(
                rhs, schema, params, resolver, control,
            )?;
            operators::binary_result_type_with_control(
                *op,
                left.as_deref(),
                right.as_deref(),
                control,
            )
        }
        ScalarExpr::UnaryMinus(inner) => {
            scalar_type_inner_with_control(inner, schema, params, resolver, control)?
                .map_or(Ok(None), |ty| {
                    operators::unary_minus_result_type_with_control(&ty, control).map(Some)
                })
        }
        ScalarExpr::Not(inner) | ScalarExpr::IsNull { expr: inner, .. } => {
            scalar_type_inner_with_control(inner, schema, params, resolver, control)?;
            copy_type(Some(&ColumnType::Boolean), control)
        }
        ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            for item in items {
                scalar_type_inner_with_control(item, schema, params, resolver, control)?;
            }
            copy_type(Some(&ColumnType::Boolean), control)
        }
        ScalarExpr::Between { expr, low, high } => {
            let value = scalar_type_inner_with_control(expr, schema, params, resolver, control)?;
            let low = scalar_type_inner_with_control(low, schema, params, resolver, control)?;
            let high = scalar_type_inner_with_control(high, schema, params, resolver, control)?;
            operators::binary_result_type_with_control(
                crate::ast::BinaryOp::GreaterEqual,
                value.as_deref(),
                low.as_deref(),
                control,
            )?;
            operators::binary_result_type_with_control(
                crate::ast::BinaryOp::LessEqual,
                value.as_deref(),
                high.as_deref(),
                control,
            )?;
            copy_type(Some(&ColumnType::Boolean), control)
        }
        ScalarExpr::InList { expr, list, .. } => {
            let needle = scalar_type_inner_with_control(expr, schema, params, resolver, control)?;
            for item in list {
                let candidate =
                    scalar_type_inner_with_control(item, schema, params, resolver, control)?;
                operators::binary_result_type_with_control(
                    crate::ast::BinaryOp::Equal,
                    needle.as_deref(),
                    candidate.as_deref(),
                    control,
                )?;
            }
            copy_type(Some(&ColumnType::Boolean), control)
        }
        ScalarExpr::InSubquery { expr, subquery, .. } => {
            let needle = scalar_type_inner_with_control(expr, schema, params, resolver, control)?;
            let candidate = resolver
                .zip(schema.physical_schema())
                .map(|(resolver, schema)| {
                    resolver.resolve_scalar_subquery_type(*subquery, schema, params)
                })
                .transpose()?
                .flatten()
                .map(|ty| control.finish(ty, None))
                .transpose()?;
            operators::binary_result_type_with_control(
                crate::ast::BinaryOp::Equal,
                needle.as_deref(),
                candidate.as_deref(),
                control,
            )?;
            copy_type(Some(&ColumnType::Boolean), control)
        }
        ScalarExpr::Exists { .. } => copy_type(Some(&ColumnType::Boolean), control),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            let simple = base.is_some();
            let base_type = base
                .as_deref()
                .map(|base| {
                    common::common_context_expression_type_with_control(
                        base, schema, params, resolver, control,
                    )
                })
                .transpose()?
                .flatten();
            let mut result = None;
            for (condition, value) in when {
                let condition_type = if simple {
                    common::common_context_expression_type_with_control(
                        condition, schema, params, resolver, control,
                    )?
                } else {
                    scalar_type_inner_with_control(condition, schema, params, resolver, control)?
                };
                if simple {
                    operators::binary_result_type_with_control(
                        crate::ast::BinaryOp::Equal,
                        base_type.as_deref(),
                        condition_type.as_deref(),
                        control,
                    )?;
                }
                result = common::merge_value_types(
                    result,
                    common::common_context_expression_type_with_control(
                        value, schema, params, resolver, control,
                    )?,
                    control,
                )?;
            }
            if let Some(value) = else_branch {
                result = common::merge_value_types(
                    result,
                    common::common_context_expression_type_with_control(
                        value, schema, params, resolver, control,
                    )?,
                    control,
                )?;
            }
            match result {
                Some(result) => common::case::case_output_type_with_control(
                    expression,
                    &result,
                    &mut |expression| {
                        scalar_type_inner_with_control(
                            expression, schema, params, resolver, control,
                        )
                    },
                    control,
                )
                .map(Some),
                result => Ok(result),
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
            if let Some(error) = binding
                .as_ref()
                .and_then(|binding| binding.resolution_error.as_ref())
            {
                return Err(error.sql_error());
            }
            if let Some(filter) = filter {
                scalar_type_inner_with_control(filter, schema, params, resolver, control)?;
            }
            if *distinct {
                for argument in args {
                    if let Some(ty) =
                        scalar_type_inner_with_control(argument, schema, params, resolver, control)?
                    {
                        operators::require_equality_operator(&ty)?;
                    }
                }
            }
            for order in order_by {
                if let Some(ty) =
                    scalar_type_inner_with_control(&order.expr, schema, params, resolver, control)?
                {
                    operators::require_ordering_operator(&ty)?;
                }
            }
            functions::builtin_function_type_with_control(
                functions::FunctionTypeCall {
                    name,
                    binding: binding.as_ref(),
                    args,
                },
                order_by,
                params,
                resolver,
                &mut |expression| {
                    scalar_type_inner_with_control(expression, schema, params, resolver, control)
                },
                control,
            )
        }
        ScalarExpr::WindowCall { name, args, spec } => {
            for expression in &spec.partition_by {
                if let Some(ty) =
                    scalar_type_inner_with_control(expression, schema, params, resolver, control)?
                {
                    operators::require_equality_operator(&ty)?;
                }
            }
            for order in &spec.order_by {
                if let Some(ty) =
                    scalar_type_inner_with_control(&order.expr, schema, params, resolver, control)?
                {
                    operators::require_ordering_operator(&ty)?;
                }
            }
            if let Some(frame) = &spec.frame {
                for bound in [&frame.start, &frame.end] {
                    match bound {
                        crate::ScalarFrameBound::Preceding(expression)
                        | crate::ScalarFrameBound::Following(expression) => {
                            scalar_type_inner_with_control(
                                expression, schema, params, resolver, control,
                            )?;
                        }
                        crate::ScalarFrameBound::UnboundedPreceding
                        | crate::ScalarFrameBound::UnboundedFollowing
                        | crate::ScalarFrameBound::CurrentRow => {}
                    }
                }
            }
            functions::builtin_function_type_with_control(
                functions::FunctionTypeCall {
                    name,
                    binding: None,
                    args,
                },
                &[],
                params,
                resolver,
                &mut |expression| {
                    scalar_type_inner_with_control(expression, schema, params, resolver, control)
                },
                control,
            )
        }
        ScalarExpr::ScalarSubquery(subquery) => {
            resolver
                .zip(schema.physical_schema())
                .map_or(Ok(None), |(resolver, schema)| {
                    resolver
                        .resolve_scalar_subquery_type(*subquery, schema, params)?
                        .map(|ty| control.finish(ty, None).map_err(Into::into))
                        .transpose()
                })
        }
        ScalarExpr::QualifiedStar(qualifier) if schema.has_qualifier(qualifier) => {
            copy_type(Some(&ColumnType::Record), control)
        }
        ScalarExpr::Star | ScalarExpr::QualifiedStar(_) | ScalarExpr::Default => Ok(None),
    }
}

fn copy_type(
    ty: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    ty.map(|ty| ty.clone_with_control(control).map_err(Into::into))
        .transpose()
}

#[cfg(test)]
mod tests;
