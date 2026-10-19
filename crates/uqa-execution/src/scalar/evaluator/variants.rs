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
use crate::scalar::state::{ColumnSlot, ConstantCast, ContextSchema};
use std::sync::OnceLock;
use uqa_core::ArrayValue;
use uqa_sql::ast::BinaryOp;
use uqa_sql::expr::RowLookup;
use uqa_sql::expr::{
    cast_value_with_type_resolution_with_control, eval_binary_values_with_engine,
    negate_value_with_control, truthy,
};

pub(super) fn array(
    items: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let element_oid = context.with_type_schema(|schema| {
        uqa_sql::type_resolution::scalar_array_element_oid_with_control(
            items,
            schema,
            context.params(),
            context.function_hook(),
            control,
        )
    })?;
    let items = evaluate_items(items, context, control)?;
    let array = ArrayValue::try_new_with_control(items, control)?.ok_or_else(|| {
        SQLError::TypeMismatch("multidimensional arrays must have matching dimensions".into())
    })?;
    let (array, memory) = array.into_parts();
    control
        .finish(
            Value::Array(array.with_element_type_oid(element_oid)),
            memory,
        )
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
        if matches!((&*left, &*right), (Value::Row(_), Value::Row(_)))
            && !matches!((lhs, rhs), (ScalarExpr::Row(_), ScalarExpr::Row(_)))
        {
            let value = uqa_sql::expr::eval_record_comparison_truth_with_engine(
                op,
                &left,
                &right,
                control,
                context.function_hook(),
            )?;
            return plain(value.map_or(Value::Null, Value::Bool), control);
        }
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
    eval_binary_values_with_engine(
        op,
        &left,
        &right,
        integer_width,
        context.function_hook(),
        control,
    )
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
    let Some(constant) = context.constant_cast(expr) else {
        return convert(expr, ty, context, control).map(|(_, converted)| converted);
    };
    match constant.get() {
        Some(ConstantCast::Retained(value)) => control.copy_value(value).map_err(Into::into),
        Some(ConstantCast::PerEvaluation) => {
            convert(expr, ty, context, control).map(|(_, converted)| converted)
        }
        None => {
            let (source, converted) = convert(expr, ty, context, control)?;
            // A concurrent first evaluation sets the same conversion of the same literal.
            let _ = constant.set(
                if immutable_constant_cast(expr, source.as_deref().map(String::as_str), ty) {
                    ConstantCast::Retained((*converted).clone())
                } else {
                    ConstantCast::PerEvaluation
                },
            );
            Ok(converted)
        }
    }
}

fn convert(
    expr: &ScalarExpr,
    ty: &str,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<(Option<Produced<String>>, Produced<Value>), SQLError> {
    #[cfg(test)]
    crate::scalar::state::CONSTANT_CAST_CONVERSIONS.with(|count| count.set(count.get() + 1));
    let source_ty = context.with_type_schema(|schema| {
        uqa_sql::type_resolution::scalar_cast_source_type_name_with_control(
            expr,
            schema,
            context.params(),
            control,
        )
    })?;
    let value = eval_scalar_inner(expr, context, control)?;
    let converted = cast_value_with_type_resolution_with_control(
        &value,
        source_ty.as_deref().map(String::as_str),
        ty,
        context.function_hook(),
        control,
    )?;
    Ok((source_ty, converted))
}

/// A literal's conversion can be retained for the rest of the execution only when no evaluation could convert it differently: the target is spelled in `pg_catalog`, which `search_path` cannot redirect, both types are builtin scalars, and the cast reads neither a catalog descriptor nor a session setting.
fn immutable_constant_cast(operand: &ScalarExpr, source: Option<&str>, target: &str) -> bool {
    let (ScalarExpr::Literal(operand) | ScalarExpr::TypedLiteral { value: operand, .. }) = operand
    else {
        return false;
    };
    if uqa_sql::expr::composites::literal::contains_records(operand)
        || uqa_sql::expr::datums::contains_datum(operand)
    {
        return false;
    }
    let Some(target) = uqa_sql::type_resolution::pg_catalog_builtin_type(target) else {
        return false;
    };
    if !uqa_sql::type_resolution::immutable_cast_type(&target) {
        return false;
    }
    source.is_none_or(|source| {
        uqa_sql::ColumnType::from_sql_name(source).is_ok_and(|source| {
            uqa_sql::type_resolution::immutable_cast_type(&source)
                && uqa_sql::type_resolution::cast_volatility(&source, &target)
                    == uqa_sql::ast::FunctionVolatility::Immutable
        })
    })
}

pub(super) fn column(
    expression: &ScalarExpr,
    name: &str,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let retained = context.column_slot(expression);
    if let Some(value) = retained.and_then(|slot| retained_value(slot, context)) {
        return control.copy_value(value).map_err(Into::into);
    }
    #[cfg(test)]
    crate::scalar::state::COLUMN_NAME_LOOKUPS.with(|count| count.set(count.get() + 1));
    if context.row_schema().is_some_and(|schema| {
        !schema.has_unqualified_column(name)
            && !schema.column_is_ambiguous(name)
            && schema.has_qualifier(name)
    }) {
        return ordinary_output(evaluate_qualified_whole_row(name, context), control);
    }
    let value = context
        .sql_context()
        .column_value_with_control(name, control)?;
    let context_schema = context
        .row_schema()
        .map_or(ContextSchema::Absent, |schema| {
            ContextSchema::Shared(schema.clone())
        });
    retain_key(retained, context, context_schema, |row| {
        row.column_key(name)
    });
    Ok(value)
}

pub(super) fn qualified_column(
    expression: &ScalarExpr,
    qualifier: &str,
    column: &str,
    context: &ScalarEvalContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let retained = context.column_slot(expression);
    if let Some(value) = retained.and_then(|slot| retained_value(slot, context)) {
        return control.copy_value(value).map_err(Into::into);
    }
    #[cfg(test)]
    crate::scalar::state::COLUMN_NAME_LOOKUPS.with(|count| count.set(count.get() + 1));
    let value = context
        .sql_context()
        .qualified_column_value_with_control(qualifier, column, control)?;
    retain_key(retained, context, ContextSchema::Unused, |row| {
        row.qualified_column_key(qualifier, column)
    });
    Ok(value)
}

/// Retain the key the rows' schema resolves a reference to. The reference named one unambiguous column of these rows, so later rows whose schema shares the index read the key without resolving the name again.
fn retain_key(
    slot: Option<&OnceLock<ColumnSlot>>,
    context: &ScalarEvalContext<'_>,
    context_schema: ContextSchema,
    key: impl FnOnce(&dyn RowLookup) -> Option<usize>,
) {
    if let (Some(slot), Some(row)) = (slot, context.row_lookup()) {
        if let Some(schema) = row.resolution_schema() {
            let _ = slot.set(ColumnSlot {
                context_schema,
                row_schema: schema.clone(),
                key_space: row.key_space(),
                key: key(row),
            });
        }
    }
}

/// The value of a reference whose key was retained for rows of the schema the row now reads, under the schema the context now carries.
fn retained_value<'a>(
    slot: &OnceLock<ColumnSlot>,
    context: &ScalarEvalContext<'a>,
) -> Option<&'a Value> {
    let resolved = slot.get()?;
    let row = context.row_lookup()?;
    if row.key_space() != resolved.key_space
        || !row.resolution_schema()?.shares_index(&resolved.row_schema)
    {
        return None;
    }
    match (&resolved.context_schema, context.row_schema()) {
        (ContextSchema::Unused, _) | (ContextSchema::Absent, None) => {}
        (ContextSchema::Shared(retained), Some(current)) if current.shares_index(retained) => {}
        _ => return None,
    }
    Some(
        resolved
            .key
            .and_then(|key| row.keyed_value(key))
            .unwrap_or(&crate::batch::NULL_VALUE),
    )
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
