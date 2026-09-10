//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluation of immutable constants without erasing their declared SQL type.

use uqa_execution::{eval_scalar, scalar_type, RowSchema, ScalarEvalContext, ScalarExpr};
use uqa_sql::ast::{ColumnType, FunctionBinding};
use uqa_sql::SQLError;

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
        _ => false,
    }
}

pub(super) fn fold_literal_expression(expression: ScalarExpr) -> Result<ScalarExpr, SQLError> {
    if literal_value(&expression).is_some() || !is_constant(&expression) {
        return Ok(expression);
    }
    let schema = RowSchema::default();
    let ty = scalar_type(&expression, &schema, &[])?;
    let context = ScalarEvalContext::new(None, &[]);
    let value = eval_scalar(&expression, &context)?;
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
