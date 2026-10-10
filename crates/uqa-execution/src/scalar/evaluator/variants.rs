//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep variant-local temporaries out of the recursive scalar dispatch frame.

use super::{
    eval_scalar_inner, evaluate_items, evaluate_qualified_whole_row, execute_exists_subquery,
    ordinary_output, plain, real_type_name, scalar_source_type, Produced, ProductionControl,
    ProductionVec, SQLError, ScalarEvalContext, ScalarExpr, Value,
};
use uqa_core::ArrayValue;
use uqa_sql::ast::BinaryOp;
use uqa_sql::expr::{
    cast_value_with_type_resolution_with_control,
    eval_binary_values_with_integer_width_with_control, negate_value_with_control, truthy,
};

pub(super) fn array(
    items: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let items = evaluate_items(items, context, control)?;
    let array = ArrayValue::try_new_with_control(items, control)?.ok_or_else(|| {
        SQLError::TypeMismatch("multidimensional arrays must have matching dimensions".into())
    })?;
    let (array, memory) = array.into_parts();
    control
        .finish(Value::Array(array), memory)
        .map_err(Into::into)
}

pub(super) fn row(
    items: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let mut fields = ProductionVec::new(*control);
    fields.reserve(items.len())?;
    let mut complete = true;
    for item in items {
        let field = context.with_type_schema(|schema| {
            uqa_sql::type_resolution::scalar_record_field_type_with_control(
                item,
                schema,
                context.params(),
                context.function_hook(),
                control,
            )
        })?;
        if let Some(field) = field {
            fields.push_copy(field)?;
        } else {
            complete = false;
        }
    }
    let values = evaluate_items(items, context, control)?;
    let row = if complete {
        uqa_core::RowValue::typed_with_control(values, fields.finish()?, control)?
    } else {
        uqa_core::RowValue::new_with_control(values, control)?
    };
    let (row, memory) = row.into_parts();
    control.finish(Value::Row(row), memory).map_err(Into::into)
}

pub(super) fn binary(
    op: BinaryOp,
    lhs: &ScalarExpr,
    rhs: &ScalarExpr,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let left = eval_scalar_inner(lhs, context, control)?;
    let right = eval_scalar_inner(rhs, context, control)?;
    if matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    ) {
        let value = uqa_sql::expr::eval_comparison_truth_with_deferred_state(
            op,
            &left,
            &right,
            control,
            context.function_hook(),
            || context.enum_binary_comparison_state(lhs),
        )?;
        return plain(value.map_or(Value::Null, Value::Bool), control);
    }
    if (matches!(*left, Value::Float(_)) || matches!(*right, Value::Float(_)))
        && matches!(
            op,
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
        )
        && scalar_source_type(lhs, context, control)?
            .map(|ty| real_type_name(&ty, control))
            .transpose()?
            .unwrap_or(false)
        && scalar_source_type(rhs, context, control)?
            .map(|ty| real_type_name(&ty, control))
            .transpose()?
            .unwrap_or(false)
    {
        let value = uqa_sql::expr::eval_float_arithmetic_with_control(
            op,
            &left,
            &right,
            uqa_sql::expr::FloatWidth::Real,
            control,
        )?;
        return plain(value, control);
    }
    // Integer operand widths constrain only integer arithmetic.
    // Other carriers keep their existing
    // numeric promotion without re-inferring the operand trees.
    let integer_width = if matches!((&*left, &*right), (Value::Int(_), Value::Int(_)))
        && matches!(
            op,
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
        ) {
        context.with_type_schema(|schema| {
            uqa_sql::scalar_integer_operation_width_with_control(
                lhs,
                rhs,
                schema,
                context.params(),
                control,
            )
        })?
    } else {
        None
    };
    eval_binary_values_with_integer_width_with_control(op, &left, &right, integer_width, control)
}

pub(super) fn unary_minus(
    inner: &ScalarExpr,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let source_ty = scalar_source_type(inner, context, control)?;
    let value = eval_scalar_inner(inner, context, control)?;
    negate_value_with_control(&value, source_ty.as_deref().map(String::as_str), control)
}

pub(super) fn not(
    inner: &ScalarExpr,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let value = eval_scalar_inner(inner, context, control)?;
    if matches!(*value, Value::Null) {
        plain(Value::Null, control)
    } else {
        plain(Value::Bool(!truthy(&value)), control)
    }
}

pub(super) fn is_null(
    expr: &ScalarExpr,
    negated: bool,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let value = eval_scalar_inner(expr, context, control)?;
    plain(
        Value::Bool(uqa_core::sql_null_test(Some(&value), negated)),
        control,
    )
}

pub(super) fn cast(
    expr: &ScalarExpr,
    ty: &str,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let source_ty = context.with_type_schema(|schema| {
        uqa_sql::type_resolution::scalar_cast_source_type_name_with_control(
            expr,
            schema,
            context.params(),
            control,
        )
    })?;
    let value = eval_scalar_inner(expr, context, control)?;
    cast_value_with_type_resolution_with_control(
        &value,
        source_ty.as_deref().map(String::as_str),
        ty,
        context.function_hook(),
        control,
    )
}

pub(super) fn column(
    name: &str,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    if context.row_schema().is_some_and(|schema| {
        !schema.has_unqualified_column(name)
            && !schema.column_is_ambiguous(name)
            && schema.has_qualifier(name)
    }) {
        ordinary_output(evaluate_qualified_whole_row(name, context), control)
    } else {
        context
            .sql_context()
            .column_value_with_control(name, control)
    }
}

pub(super) fn exists(
    subquery: super::SubqueryId,
    negated: bool,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let exists = execute_exists_subquery(subquery, context)?;
    plain(Value::Bool(if negated { !exists } else { exists }), control)
}

pub(super) fn in_subquery(
    expr: &ScalarExpr,
    subquery: super::SubqueryId,
    negated: bool,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let needle = eval_scalar_inner(expr, context, control)?;
    let found = super::execute_in_subquery(subquery, &needle, context)?;
    plain(
        found.map_or(Value::Null, |found| {
            Value::Bool(if negated { !found } else { found })
        }),
        control,
    )
}
