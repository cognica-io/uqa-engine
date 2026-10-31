//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constant-folding eligibility borrows scalar owners and admits every temporary type observation before constructing it.

use super::{
    BinaryOp, ColumnType, FunctionTypeResolver, FunctionVolatility, Produced, ProductionControl,
    RowSchema, SQLError, ScalarExpr,
};
use uqa_sql::routines::declaration::RoutineTypeCatalog;

fn semantic<T>(result: Result<T, SQLError>) -> Result<Option<T>, SQLError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(None),
    }
}

pub(super) fn cast_type(
    name: &str,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    if let Some(types) = types {
        return semantic(types.resolve_catalog_column_type_name_with_control(name, control));
    }
    uqa_sql::type_resolution::pg_catalog_builtin_type_with_control(name, control)
}

pub(super) fn scalar_type(
    expression: &ScalarExpr,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    uqa_sql::scalar_type_with_resolver_and_control(
        expression,
        &RowSchema::default(),
        &[],
        resolver,
        control,
    )
}

pub(super) fn normalized_name<'a>(
    name: &'a str,
    control: &ProductionControl<'_>,
) -> Result<uqa_sql::semantics::FunctionName<'a>, SQLError> {
    Ok(uqa_sql::semantics::lowercase_function_name_with_control(
        name, control,
    )?)
}

fn immutable_binary(
    op: BinaryOp,
    left: &ScalarExpr,
    right: &ScalarExpr,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    // Observe both operands before deciding whether ordinary analysis should report a semantic error.
    let left = semantic(scalar_type(left, resolver, control))?;
    let right = semantic(scalar_type(right, resolver, control))?;
    let (Some(left), Some(right)) = (left, right) else {
        return Ok(true);
    };
    Ok(semantic(
        uqa_sql::type_resolution::binary_operator_volatility_with_control(
            op,
            left.as_deref(),
            right.as_deref(),
            control,
        ),
    )?
    .is_none_or(|volatility| volatility == FunctionVolatility::Immutable))
}

pub(super) fn is_constant(
    expression: &ScalarExpr,
    types: Option<&dyn RoutineTypeCatalog>,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    if uqa_sql::catalog::collations::expression::scalar_clause(expression)
        .is_ok_and(|clause| clause.is_some())
    {
        return Ok(false);
    }
    let constant = |expression: &ScalarExpr| is_constant(expression, types, resolver, control);
    Ok(match expression {
        ScalarExpr::RetainedLiteral(literal) => immutable_retained(literal, types, control)?,
        ScalarExpr::Collated { expr, .. } => constant(expr)?,
        ScalarExpr::Literal(value) => super::immutable_literal(value),
        ScalarExpr::TypedLiteral {
            value,
            ty,
            bound_type,
            ..
        } => {
            super::immutable_literal(value)
                && (bound_type.is_some() || cast_type(ty, types, control)?.is_some())
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items
            .iter()
            .try_fold(true, |all, item| Ok::<_, SQLError>(all && constant(item)?))?,
        ScalarExpr::Cast { expr, ty, .. } => immutable_cast(expr, ty, types, resolver, control)?,
        ScalarExpr::CompositeRow { .. } => false,
        ScalarExpr::Binary { op, lhs, rhs } => {
            constant(lhs)? && constant(rhs)? && immutable_binary(*op, lhs, rhs, resolver, control)?
        }
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. } => constant(inner)?,
        ScalarExpr::Between { expr, low, high } => {
            constant(expr)?
                && constant(low)?
                && constant(high)?
                && immutable_binary(BinaryOp::LessEqual, low, expr, resolver, control)?
                && immutable_binary(BinaryOp::LessEqual, expr, high, resolver, control)?
        }
        ScalarExpr::InList { expr, list, .. } => {
            constant(expr)?
                && list.iter().try_fold(true, |all, item| {
                    Ok::<_, SQLError>(
                        all && constant(item)?
                            && immutable_binary(BinaryOp::Equal, expr, item, resolver, control)?,
                    )
                })?
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().map(constant).transpose()?.unwrap_or(true)
                && when.iter().try_fold(true, |all, (condition, result)| {
                    Ok::<_, SQLError>(all && constant(condition)? && constant(result)?)
                })?
                && else_branch
                    .as_deref()
                    .map(constant)
                    .transpose()?
                    .unwrap_or(true)
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } if super::is_coalesce(name, binding.as_ref()) => args
            .iter()
            .try_fold(true, |all, item| Ok::<_, SQLError>(all && constant(item)?))?,
        ScalarExpr::Func {
            name,
            binding: Some(binding),
            args,
            distinct: false,
            order_by,
            filter: None,
            ..
        } if binding.builtin
            && !uqa_sql::semantics::function_has_implicit_row_input(binding)
            && order_by.is_empty()
            && !uqa_sql::semantics::is_builtin_aggregate(expression) =>
        {
            let normalized = normalized_name(name, control)?;
            let name = uqa_sql::semantics::builtin_function_dispatch_name(&normalized);
            !uqa_sql::semantics::sets::validation::builtin_returns_set(&name)
                && uqa_sql::semantics::volatility::builtin_function_volatility(
                    &normalized,
                    Some(binding),
                    args.len(),
                ) == FunctionVolatility::Immutable
                && args
                    .iter()
                    .try_fold(true, |all, item| Ok::<_, SQLError>(all && constant(item)?))?
        }
        _ => false,
    })
}

fn immutable_cast(
    expr: &ScalarExpr,
    ty: &str,
    types: Option<&dyn RoutineTypeCatalog>,
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    if !is_constant(expr, types, resolver, control)? {
        return Ok(false);
    }
    let Some(target) = cast_type(ty, types, control)? else {
        return Ok(false);
    };
    if !uqa_sql::type_resolution::immutable_cast_type(&target) {
        return Ok(false);
    }
    let Some(source) = semantic(scalar_type(expr, resolver, control))? else {
        return Ok(false);
    };
    Ok(match source {
        Some(source) => {
            uqa_sql::type_resolution::cast_volatility_with_control(&source, &target, control)?
                == FunctionVolatility::Immutable
        }
        None => true,
    })
}

fn immutable_retained(
    literal: &uqa_sql::ir::RetainedScalarLiteral,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    if !literal
        .value
        .resident_value()
        .is_none_or(super::immutable_literal)
    {
        return Ok(false);
    }
    Ok(match literal.declaration.as_deref() {
        Some(name) if literal.ty.is_none() => cast_type(name, types, control)?.is_some(),
        _ => true,
    })
}
