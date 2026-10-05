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
use uqa_core::Value;

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
                element = common::merge_value_types_in(
                    common::CommonTypeContext::Array,
                    element,
                    common::common_context_expression_type_with_control(
                        item, schema, params, resolver, control,
                    )?,
                    control,
                )?;
            }
            let element =
                element.map_or_else(|| ColumnType::Text.clone_with_control(control), Ok)?;
            common::read_unknown_literals(items.iter(), &element)?;
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
            // An operator's `unknown` literal is read by the input function of the operand type the selected operator declares, as `coerce_type` reads it when the operator is resolved: a `regclass` column compared with `'name'` reads the literal as `oid`, and `1 + '1'` reads `'1'` as an integer.
            if left.is_some() != right.is_some()
                && left
                    .as_deref()
                    .into_iter()
                    .chain(right.as_deref())
                    .all(|ty| {
                        !matches!(
                            common::base_type(ty),
                            ColumnType::Vector(_) | ColumnType::Tensor(_)
                        )
                    })
            {
                let selected = operators::binary_operator_types_with_control(
                    *op,
                    left.as_deref(),
                    right.as_deref(),
                    control,
                )?;
                let [left_target, right_target, _] = &*selected;
                common::read_unknown_literals(std::iter::once(lhs.as_ref()), left_target)?;
                common::read_unknown_literals(std::iter::once(rhs.as_ref()), right_target)?;
            }
            operators::binary_result_type_with_control(
                *op,
                left.as_deref(),
                right.as_deref(),
                control,
            )
        }
        ScalarExpr::UnaryMinus(inner) => {
            // The negations of the numeric types and of interval all accept an `unknown` operand, so `-'1'` selects none of them, as `oper_select_candidate` reports.
            if matches!(inner.as_ref(), ScalarExpr::Literal(Value::Str(_))) {
                return Err(super::ambiguous_prefix_operator("-", "unknown"));
            }
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
        // `unknown` operands take the type selected by each comparison operator.
        ScalarExpr::Between { expr, low, high } => {
            let value = common::common_context_expression_type_with_control(
                expr, schema, params, resolver, control,
            )?;
            let low = common::common_context_expression_type_with_control(
                low, schema, params, resolver, control,
            )?;
            let high = common::common_context_expression_type_with_control(
                high, schema, params, resolver, control,
            )?;
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
            let needle = common::common_context_expression_type_with_control(
                expr, schema, params, resolver, control,
            )?;
            let mut candidates = Vec::with_capacity(list.len());
            for item in list {
                candidates.push(common::common_context_expression_type_with_control(
                    item, schema, params, resolver, control,
                )?);
            }
            // `transformAExprIn` compares the needle with the list coerced to the inputs' common type; without one, each item is compared separately.
            let inputs = std::iter::once(needle.as_deref())
                .chain(candidates.iter().map(Option::as_deref))
                .collect::<Vec<_>>();
            match common::select_common_input_type_with_control(&inputs, control)? {
                Some(common) => {
                    common::read_unknown_literals(
                        std::iter::once(expr.as_ref()).chain(list.iter()),
                        &common,
                    )?;
                    operators::binary_result_type_with_control(
                        crate::ast::BinaryOp::Equal,
                        needle.as_deref(),
                        Some(&common),
                        control,
                    )?;
                }
                None => {
                    for candidate in &candidates {
                        operators::binary_result_type_with_control(
                            crate::ast::BinaryOp::Equal,
                            needle.as_deref(),
                            candidate.as_deref(),
                            control,
                        )?;
                    }
                }
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
            let mut results = Vec::with_capacity(when.len() + 1);
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
                results.push(common::common_context_expression_type_with_control(
                    value, schema, params, resolver, control,
                )?);
            }
            if let Some(value) = else_branch {
                // `transformCaseExpr` selects the result type with the ELSE result first.
                results.insert(
                    0,
                    common::common_context_expression_type_with_control(
                        value, schema, params, resolver, control,
                    )?,
                );
            }
            let mut result = None;
            for ty in results {
                result = common::merge_value_types_in(
                    common::CommonTypeContext::Case,
                    result,
                    ty,
                    control,
                )?;
            }
            // `select_common_type` resolves results that are all `unknown` literals to text.
            if result.is_none()
                && when
                    .iter()
                    .map(|(_, value)| value)
                    .chain(else_branch.as_deref())
                    .all(super::is_unknown_literal)
            {
                result = Some(ColumnType::Text.clone_with_control(control)?);
            }
            if let Some(result) = &result {
                common::read_unknown_literals(
                    when.iter()
                        .map(|(_, value)| value)
                        .chain(else_branch.as_deref()),
                    result,
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
            if binding.as_ref().and_then(|binding| binding.dispatch)
                == Some(crate::ast::FunctionDispatch::FieldSelect)
            {
                return field_selection_type(args, schema, params, resolver, control);
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
        ScalarExpr::WindowCall {
            name,
            args,
            spec,
            filter,
            ..
        } => {
            if let Some(filter) = filter {
                scalar_type_inner_with_control(filter, schema, params, resolver, control)?;
            }
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

/// `(expression).field`: a whole-row reference selects its relation's column, a row constructor its `fN` field, and any other expression its composite type's attribute.
pub(super) fn field_selection_type(
    args: &[ScalarExpr],
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    use super::field_selection;
    let [base, ScalarExpr::Literal(uqa_core::Value::Str(field))] = args else {
        return Err(SQLError::Internal(
            "field selection takes an expression and a field name".into(),
        ));
    };
    let whole_row = match base {
        ScalarExpr::Column(qualifier)
            if !schema.has_unqualified_column(qualifier)
                && !schema.column_is_ambiguous(qualifier)
                && schema.has_qualifier(qualifier) =>
        {
            Some(qualifier)
        }
        ScalarExpr::QualifiedStar(qualifier) if schema.has_qualifier(qualifier) => Some(qualifier),
        _ => None,
    };
    if let Some(qualifier) = whole_row {
        if !schema.has_qualified_column(qualifier, field) {
            return Err(field_selection::missing_relation_column(qualifier, field));
        }
        return copy_type(schema.qualified_type(qualifier, field), control);
    }
    if let ScalarExpr::Row(items) = base {
        let items = items
            .iter()
            .map(|item| {
                scalar_type_inner_with_control(item, schema, params, resolver, control)
                    .map(|ty| ty.map(|ty| (*ty).clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        return copy_type(
            field_selection::row_field_type(&items, field)?.as_ref(),
            control,
        );
    }
    if let ScalarExpr::Literal(value) = base {
        if let Some(field_type) = field_selection::literal_field_type(value, field) {
            return copy_type(field_type?.as_ref(), control);
        }
    }
    let base = scalar_type_inner_with_control(base, schema, params, resolver, control)?;
    copy_type(
        field_selection::value_field_type(base.as_deref(), field, resolver)?.as_ref(),
        control,
    )
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
