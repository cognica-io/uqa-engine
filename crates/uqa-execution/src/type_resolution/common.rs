//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::Value;
use uqa_sql::ast::ColumnType;
use uqa_sql::{SQLError, SQLParam};

use crate::{RowSchema, ScalarExpr};

use super::{scalar_type_inner, FunctionTypeResolver};

pub(super) fn local_routine_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower
        .strip_prefix("pg_catalog.")
        .unwrap_or(&lower)
        .to_string()
}

pub(super) fn numeric_type() -> ColumnType {
    ColumnType::Numeric {
        precision: None,
        scale: None,
    }
}

pub(super) fn base_type(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    ty
}

pub fn values_column_types(
    rows: &[Vec<ScalarExpr>],
    params: &[SQLParam],
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let width = rows.first().map_or(0, Vec::len);
    let empty = RowSchema::default();
    let mut types = vec![None; width];
    for row in rows {
        if row.len() != width {
            return Err(SQLError::TypeMismatch(
                "VALUES lists must all be the same length".into(),
            ));
        }
        for (position, expression) in row.iter().enumerate() {
            types[position] = merge_optional_types(
                types[position].take(),
                common_context_expression_type(expression, &empty, params, None)?,
            )?;
        }
    }
    Ok(types
        .into_iter()
        .map(|ty| ty.or(Some(ColumnType::Text)))
        .collect())
}

/// Resolve an expression participating in `PostgreSQL`'s common-type selection. Bare string and NULL literals retain the parser's `unknown` type until the surrounding VALUES, set operation, CASE, or array context selects a concrete type.
pub fn common_context_expression_type(
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<Option<ColumnType>, SQLError> {
    if matches!(expression, ScalarExpr::Literal(Value::Str(_) | Value::Null)) {
        return Ok(None);
    }
    scalar_type_inner(expression, schema, params, resolver)
}

/// Preserve parser-level `unknown` identity for fixed built-in overload selection.
#[doc(hidden)]
pub fn effective_overload_argument_type(
    expression: &ScalarExpr,
    resolved: Option<ColumnType>,
) -> Option<ColumnType> {
    if matches!(expression, ScalarExpr::Literal(Value::Str(_) | Value::Null))
        || matches!(expression, ScalarExpr::Param(_))
            && matches!(resolved.as_ref(), Some(ColumnType::Text))
    {
        None
    } else {
        resolved
    }
}

/// Preserve an explicitly typed scalar parameter while retaining the legacy `unknown` treatment of untyped text-valued [`SQLParam::Scalar`] parameters.
#[doc(hidden)]
pub fn effective_overload_argument_type_with_params(
    expression: &ScalarExpr,
    resolved: Option<ColumnType>,
    params: &[SQLParam],
) -> Option<ColumnType> {
    if let ScalarExpr::Param(index) = expression {
        if index
            .checked_sub(1)
            .and_then(|index| params.get(index))
            .is_some_and(|parameter| parameter.declared_scalar_type().is_some())
        {
            return resolved;
        }
    }
    effective_overload_argument_type(expression, resolved)
}

pub(super) fn parameter_type(parameter: &SQLParam) -> Option<ColumnType> {
    match parameter {
        SQLParam::Scalar(value) => value_type(value),
        SQLParam::TypedScalar { ty, .. } => Some(ty.clone()),
        SQLParam::Vector(values) => u32::try_from(values.len()).ok().map(ColumnType::Vector),
        SQLParam::Tensor(values) => values
            .first()
            .and_then(|values| u32::try_from(values.len()).ok())
            .map(ColumnType::Tensor),
    }
}

pub(super) fn value_type(value: &Value) -> Option<ColumnType> {
    match value {
        Value::Null | Value::Map(_) => None,
        Value::Row(_) | Value::Record(_) => Some(ColumnType::Record),
        Value::Bool(_) => Some(ColumnType::Boolean),
        Value::Int(value) if i32::try_from(*value).is_ok() => Some(ColumnType::Integer),
        Value::Int(_) => Some(ColumnType::BigInteger),
        Value::Float(_) => Some(ColumnType::DoublePrecision),
        Value::Decimal(_) => Some(numeric_type()),
        Value::Str(_) => Some(ColumnType::Text),
        Value::FixedChar(value) => u32::try_from(value.chars().count())
            .ok()
            .map(ColumnType::Character),
        Value::Bytes(_) => Some(ColumnType::Bytea),
        Value::Temporal(value) => Some(match value {
            uqa_core::TemporalValue::Date { .. } => ColumnType::Date,
            uqa_core::TemporalValue::Time { .. } => ColumnType::Time,
            uqa_core::TemporalValue::TimeTz { .. } => ColumnType::TimeTz,
            uqa_core::TemporalValue::Timestamp { .. } => ColumnType::Timestamp,
            uqa_core::TemporalValue::TimestampTz { .. } => ColumnType::TimestampTz,
            uqa_core::TemporalValue::Interval { .. } => ColumnType::Interval,
        }),
        Value::Json(_) => Some(ColumnType::Json),
        Value::JsonB(_) => Some(ColumnType::JsonB),
        Value::Array(array) => {
            let mut element = None;
            merge_array_element_types(array.elements(), &mut element)?;
            element.map(|element| ColumnType::Array(Box::new(element)))
        }
        Value::List(values) => {
            let mut element = None;
            for value in values {
                element = merge_optional_types(element, value_type(value)).ok()?;
            }
            element.map(|element| ColumnType::Array(Box::new(element)))
        }
    }
}

fn merge_array_element_types(values: &[Value], element: &mut Option<ColumnType>) -> Option<()> {
    for value in values {
        if let Value::List(nested) = value {
            merge_array_element_types(nested, element)?;
        } else {
            *element = merge_optional_types(element.take(), value_type(value)).ok()?;
        }
    }
    Some(())
}

pub(super) fn merge_optional_types(
    left: Option<ColumnType>,
    right: Option<ColumnType>,
) -> Result<Option<ColumnType>, SQLError> {
    match (left, right) {
        (None, other) | (other, None) => Ok(other),
        (Some(left), Some(right)) => common_type(&left, &right).map(Some),
    }
}

pub fn common_type(left: &ColumnType, right: &ColumnType) -> Result<ColumnType, SQLError> {
    if left == right {
        return Ok(left.clone());
    }
    if matches!(left, ColumnType::Domain { .. }) || matches!(right, ColumnType::Domain { .. }) {
        return common_type(base_type(left), base_type(right));
    }
    if let Some(numeric) = common_numeric_type(left, right) {
        return Ok(numeric);
    }
    if matches!(left, ColumnType::Oid) && is_integral_type(right)
        || matches!(right, ColumnType::Oid) && is_integral_type(left)
    {
        return Ok(ColumnType::Oid);
    }
    if left.is_character_string() && right.is_character_string() {
        return Ok(match left {
            ColumnType::Bpchar | ColumnType::Character(_) => ColumnType::Bpchar,
            ColumnType::Varchar(_) => ColumnType::Varchar(None),
            ColumnType::Name => ColumnType::Name,
            _ => ColumnType::Text,
        });
    }
    match (left, right) {
        (ColumnType::Date, ColumnType::Timestamp) | (ColumnType::Timestamp, ColumnType::Date) => {
            Ok(ColumnType::Timestamp)
        }
        (ColumnType::Date | ColumnType::Timestamp, ColumnType::TimestampTz)
        | (ColumnType::TimestampTz, ColumnType::Date | ColumnType::Timestamp) => {
            Ok(ColumnType::TimestampTz)
        }
        (ColumnType::Array(left), ColumnType::Array(right)) => {
            common_type(left, right).map(|element| ColumnType::Array(Box::new(element)))
        }
        _ => Err(SQLError::TypeMismatch(format!(
            "types {} and {} cannot be matched",
            left.sql_name(),
            right.sql_name()
        ))),
    }
}

fn is_integral_type(ty: &ColumnType) -> bool {
    matches!(
        base_type(ty),
        ColumnType::SmallInteger | ColumnType::Integer | ColumnType::BigInteger
    )
}

pub(super) fn common_numeric_type(left: &ColumnType, right: &ColumnType) -> Option<ColumnType> {
    let rank = numeric_rank(left)?.max(numeric_rank(right)?);
    Some(match rank {
        0 => ColumnType::SmallInteger,
        1 => ColumnType::Integer,
        2 => ColumnType::BigInteger,
        3 => numeric_type(),
        4 => ColumnType::Real,
        _ => ColumnType::DoublePrecision,
    })
}

pub(super) fn numeric_rank(ty: &ColumnType) -> Option<u8> {
    match ty {
        ColumnType::SmallInteger => Some(0),
        ColumnType::Integer => Some(1),
        ColumnType::BigInteger => Some(2),
        ColumnType::Numeric { .. } => Some(3),
        ColumnType::Real => Some(4),
        ColumnType::DoublePrecision => Some(5),
        _ => None,
    }
}
