//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary and retained callers share scalar inference while every synthesized type keeps its constructor lease.

use super::{cast_compatibility, common, operators, qualified_column, FunctionTypeResolver};
use crate::{ast::ColumnType, schema::ScalarTypeSchema, SQLError, SQLParam, ScalarExpr};
use uqa_core::memory::{Produced, ProductionControl};
use uqa_core::Value;

mod nodes;

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
    if let Some(ty) = schema.observed_expression_type(expression) {
        return copy_type(ty, control);
    }
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
                copy_type(
                    schema.whole_row_type(column).or(Some(&ColumnType::Record)),
                    control,
                )
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
        ScalarExpr::RetainedLiteral(literal) => copy_type(literal.ty.as_ref(), control),
        ScalarExpr::Collated { .. } => {
            nodes::collated_type_with_control(expression, schema, params, resolver, control)
        }
        ScalarExpr::TypedLiteral {
            bound_type: Some(ty),
            ..
        } => copy_type(Some(ty), control),
        ScalarExpr::TypedLiteral { ty, .. } => {
            let target = resolved_type_name(ty, resolver, control)?;
            Ok(target)
        }
        ScalarExpr::Param(index) => index
            .checked_sub(1)
            .and_then(|index| params.get(index))
            .map(|parameter| common::parameter_type_with_control(parameter, control))
            .transpose()
            .map(Option::flatten),
        ScalarExpr::Cast { .. } => {
            nodes::cast_type_with_control(expression, schema, params, resolver, control)
        }
        ScalarExpr::Array(items) => {
            array_element_type_with_control(items, schema, params, resolver, control)?
                .map(|element| ColumnType::array_with_control(element, control).map_err(Into::into))
                .transpose()
        }
        ScalarExpr::CompositeRow {
            binding,
            bound_type,
            ..
        } => {
            if let Some(resolver) = resolver {
                let ty = resolver.resolve_type_name(&binding.ty)?.ok_or_else(|| {
                    SQLError::Internal("composite constructor type disappeared".into())
                })?;
                Ok(Some(control.finish(ty, None)?))
            } else if let Some(ty) = bound_type {
                copy_type(Some(ty), control)
            } else {
                Err(SQLError::Internal(
                    "composite constructor result type was not bound".into(),
                ))
            }
        }
        ScalarExpr::Row(items) => {
            for item in items {
                scalar_type_inner_with_control(item, schema, params, resolver, control)?;
            }
            copy_type(Some(&ColumnType::Record), control)
        }
        ScalarExpr::Binary { .. } => {
            nodes::binary_type_with_control(expression, schema, params, resolver, control)
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
        ScalarExpr::Between { .. } => {
            nodes::between_type_with_control(expression, schema, params, resolver, control)
        }
        ScalarExpr::InList { .. } => {
            nodes::membership_type_with_control(expression, schema, params, resolver, control)
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
        ScalarExpr::Case { .. } => {
            nodes::conditional_type_with_control(expression, schema, params, resolver, control)
        }
        ScalarExpr::Func { .. } | ScalarExpr::WindowCall { .. } => {
            super::function_inputs::call_type_with_control(
                expression,
                schema,
                params,
                resolver,
                &mut |expression| {
                    scalar_type_inner_with_control(expression, schema, params, resolver, control)
                },
                control,
                true,
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
        ScalarExpr::QualifiedStar(qualifier) if schema.has_qualifier(qualifier) => copy_type(
            schema
                .whole_row_type(qualifier)
                .or(Some(&ColumnType::Record)),
            control,
        ),
        ScalarExpr::Star | ScalarExpr::QualifiedStar(_) | ScalarExpr::Default => Ok(None),
    }
}

pub(super) fn array_element_type_with_control(
    items: &[ScalarExpr],
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    if items.is_empty() {
        return Ok(None);
    }
    let mut selection = common::CommonInputType::new();
    for item in items {
        let ty = common::common_context_expression_type_with_control(
            item, schema, params, resolver, control,
        )?;
        if ty.is_some() || common::unknown_input(item) {
            selection.include_owned(common::CommonTypeContext::Array, ty, control)?;
        }
    }
    let element = selection.finish(control)?;
    common::read_unknown_literals(items.iter(), &element)?;
    Ok(Some(element))
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
    if let Some(qualifier) = field_selection::whole_row_qualifier(base, schema) {
        if !schema.has_qualified_column(qualifier, field) {
            return Err(field_selection::missing_relation_column(qualifier, field));
        }
        return copy_type(schema.qualified_type(qualifier, field), control);
    }
    let mut origin = base;
    while let ScalarExpr::Cast { expr, .. } = origin {
        origin = expr;
    }
    if let ScalarExpr::Row(expressions) = origin {
        let items = expressions
            .iter()
            .map(|item| {
                scalar_type_inner_with_control(item, schema, params, resolver, control)
                    .map(|ty| ty.map(|ty| (*ty).clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !std::ptr::eq(origin, base) {
            let observed = super::function_inputs::CallInputTypes::new(
                schema,
                expressions
                    .iter()
                    .zip(&items)
                    .map(|(item, ty)| (item, ty.as_ref()))
                    .collect(),
            );
            // Validate casts in their original inner-to-outer order using the field types already observed above.
            let ty = scalar_type_inner_with_control(base, &observed, params, resolver, control)?;
            if ty.as_deref() != Some(&ColumnType::Record) {
                return copy_type(
                    field_selection::value_field_type(ty.as_deref(), field, resolver)?.as_ref(),
                    control,
                );
            }
        }
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
    let ty = scalar_type_inner_with_control(base, schema, params, resolver, control)?;
    let record = ty.as_deref() == Some(&ColumnType::Record);
    let reference = if record
        && !std::ptr::eq(origin, base)
        && field_selection::is_column_reference(origin)
        && scalar_type_inner_with_control(origin, schema, params, resolver, control)?.as_deref()
            == Some(&ColumnType::Record)
    {
        origin
    } else {
        base
    };
    if let Some(descriptor) = schema
        .physical_schema()
        .and_then(|schema| schema.input_record_metadata(reference))
        .and_then(|metadata| metadata.descriptor.as_ref())
    {
        let position = descriptor
            .position(field)
            .ok_or_else(|| field_selection::undefined_record_field(field))?;
        return copy_type(descriptor.types[position].as_ref(), control);
    }
    if record && field_selection::is_column_reference(reference) {
        return Err(field_selection::unregistered_record());
    }
    copy_type(
        field_selection::value_field_type(ty.as_deref(), field, resolver)?.as_ref(),
        control,
    )
}

pub(super) fn resolved_type_name(
    name: &str,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    if let Some(resolver) = resolver {
        if let Some(ty) = resolver.resolve_type_name(name)? {
            control.check()?;
            return control.finish(ty, None).map(Some).map_err(Into::into);
        }
        control.check()?;
    }
    ColumnType::from_sql_name_with_control(name, control).map(Some)
}

pub(super) fn copy_type(
    ty: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    ty.map(|ty| ty.clone_with_control(control).map_err(Into::into))
        .transpose()
}

#[cfg(test)]
mod tests;
