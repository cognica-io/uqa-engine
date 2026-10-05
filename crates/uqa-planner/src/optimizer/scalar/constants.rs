//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluation of immutable constants without erasing their declared SQL type.

use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::SQLError;
use uqa_sql::{scalar_type, RowSchema, ScalarExpr};

use super::Value;

pub(super) fn literal_value(expression: &ScalarExpr) -> Option<&Value> {
    match expression {
        ScalarExpr::Literal(value) | ScalarExpr::TypedLiteral { value, .. } => Some(value),
        _ => None,
    }
}

pub(super) fn is_coalesce(name: &str, binding: Option<&FunctionBinding>) -> bool {
    name.eq_ignore_ascii_case("coalesce") && binding.is_none_or(|binding| binding.builtin)
}

fn immutable_cast_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Array(element) => immutable_cast_type(element),
        ColumnType::SmallInteger
        | ColumnType::Integer
        | ColumnType::BigInteger
        | ColumnType::Oid
        | ColumnType::Xid
        | ColumnType::Boolean
        | ColumnType::Text
        | ColumnType::Name
        | ColumnType::Uuid
        | ColumnType::Varchar(_)
        | ColumnType::Bpchar
        | ColumnType::Character(_)
        | ColumnType::Real
        | ColumnType::DoublePrecision
        | ColumnType::Numeric { .. }
        | ColumnType::Json
        | ColumnType::JsonB
        | ColumnType::Bytea
        | ColumnType::InternalChar => true,
        _ => false,
    }
}

fn is_constant(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Literal(_) => true,
        ScalarExpr::TypedLiteral { ty, bound_type, .. } => {
            bound_type.is_some() || ColumnType::from_sql_name(ty).is_ok()
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().all(is_constant),
        ScalarExpr::Cast { expr, ty } => {
            is_constant(expr)
                && ColumnType::from_sql_name(ty).is_ok_and(|ty| immutable_cast_type(&ty))
        }
        ScalarExpr::Binary { lhs, rhs, .. } => is_constant(lhs) && is_constant(rhs),
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. } => is_constant(inner),
        ScalarExpr::Between { expr, low, high } => {
            is_constant(expr) && is_constant(low) && is_constant(high)
        }
        ScalarExpr::InList { expr, list, .. } => is_constant(expr) && list.iter().all(is_constant),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_none_or(is_constant)
                && when
                    .iter()
                    .all(|(condition, value)| is_constant(condition) && is_constant(value))
                && else_branch.as_deref().is_none_or(is_constant)
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } if is_coalesce(name, binding.as_ref()) => args.iter().all(is_constant),
        ScalarExpr::Func {
            name,
            binding: Some(binding),
            args,
            distinct: false,
            order_by,
            filter: None,
        } if binding.builtin
            && order_by.is_empty()
            && !uqa_sql::semantics::is_builtin_aggregate(expression)
            && !uqa_sql::semantics::sets::validation::builtin_returns_set(
                &uqa_sql::semantics::builtin_function_dispatch_name(&name.to_ascii_lowercase()),
            )
            && uqa_sql::semantics::volatility::builtin_function_volatility(
                name,
                Some(binding),
                args.len(),
            ) == uqa_sql::ast::FunctionVolatility::Immutable =>
        {
            args.iter().all(is_constant)
        }
        _ => false,
    }
}

pub(super) fn fold_literal_expression(
    expression: ScalarExpr,
    evaluate: crate::optimizer::ConstantEvaluator,
) -> Result<ScalarExpr, SQLError> {
    if matches!(&expression, ScalarExpr::Func { binding: Some(binding), .. }
        if matches!(binding.dispatch, Some(uqa_sql::ast::FunctionDispatch::NamedArgument | uqa_sql::ast::FunctionDispatch::VariadicArgument)))
    {
        // Argument markers carry syntax for their enclosing call; only that call evaluates them as arguments.
        return Ok(expression);
    }
    if literal_value(&expression).is_some() || !is_constant(&expression) {
        return Ok(expression);
    }
    let schema = RowSchema::default();
    let ty = scalar_type(&expression, &schema, &[])?;
    // Keep operator-selected casts before evaluation can replace the expression with a literal, including PostgreSQL unknown string inputs.
    let expression = uqa_sql::bind_type_introspection(expression, &schema, &[]);
    let value = evaluate(&expression)?;
    let literal = ScalarExpr::Literal(value.clone());
    if !matches!(expression, ScalarExpr::Cast { .. }) && scalar_type(&literal, &schema, &[])? == ty
    {
        return Ok(literal);
    }
    Ok(match ty {
        Some(ty) => ScalarExpr::TypedLiteral {
            value,
            ty: ty.sql_name(),
            bound_type: Some(ty),
            parameter_index: None,
        },
        None => literal,
    })
}

#[cfg(test)]
mod tests;
