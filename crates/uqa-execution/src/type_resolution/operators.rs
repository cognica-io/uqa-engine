//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_sql::ast::{BinaryOp, ColumnType};
use uqa_sql::SQLError;

use super::common::{base_type, common_numeric_type, merge_optional_types, numeric_rank};

pub(super) fn unary_minus_result_type(ty: &ColumnType) -> Result<ColumnType, SQLError> {
    match base_type(ty) {
        ty @ (ColumnType::SmallInteger
        | ColumnType::Integer
        | ColumnType::BigInteger
        | ColumnType::Real
        | ColumnType::DoublePrecision
        | ColumnType::Numeric { .. }
        | ColumnType::Interval) => Ok(ty.clone()),
        other => Err(SQLError::TypeMismatch(format!(
            "operator does not exist: - {}",
            other.sql_name()
        ))),
    }
}

pub(super) fn binary_result_type(
    op: BinaryOp,
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
) -> Result<Option<ColumnType>, SQLError> {
    if matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    ) {
        return Ok(Some(ColumnType::Boolean));
    }
    let (Some(left), Some(right)) = (left, right) else {
        return merge_optional_types(left.cloned(), right.cloned());
    };
    let left = base_type(left);
    let right = base_type(right);
    if let Some(ty) = temporal_binary_result_type(op, left, right) {
        return Ok(Some(ty));
    }
    if let Some(ty) = common_numeric_type(left, right) {
        return Ok(Some(ty));
    }
    if matches!(left, ColumnType::JsonB)
        && matches!(op, BinaryOp::Subtract)
        && (right.is_character_string()
            || matches!(right, ColumnType::SmallInteger | ColumnType::Integer)
            || matches!(right, ColumnType::Array(element) if element.is_character_string()))
    {
        return Ok(Some(ColumnType::JsonB));
    }
    Err(SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!(
            "operator does not exist: {} {} {}",
            left.sql_name(),
            binary_operator_name(op),
            right.sql_name()
        ),
    })
}

fn temporal_binary_result_type(
    op: BinaryOp,
    left: &ColumnType,
    right: &ColumnType,
) -> Option<ColumnType> {
    use ColumnType as T;
    match (left, right, op) {
        (T::Date, T::Date, BinaryOp::Subtract) => Some(T::Integer),
        (T::Date, T::SmallInteger | T::Integer, BinaryOp::Add | BinaryOp::Subtract)
        | (T::SmallInteger | T::Integer, T::Date, BinaryOp::Add) => Some(T::Date),
        (T::Date | T::Timestamp, T::Interval, BinaryOp::Add | BinaryOp::Subtract)
        | (T::Interval, T::Date | T::Timestamp, BinaryOp::Add) => Some(T::Timestamp),
        (T::TimestampTz, T::Interval, BinaryOp::Add | BinaryOp::Subtract)
        | (T::Interval, T::TimestampTz, BinaryOp::Add) => Some(T::TimestampTz),
        (T::Time, T::Interval, BinaryOp::Add | BinaryOp::Subtract)
        | (T::Interval, T::Time, BinaryOp::Add) => Some(T::Time),
        (T::TimeTz, T::Interval, BinaryOp::Add | BinaryOp::Subtract)
        | (T::Interval, T::TimeTz, BinaryOp::Add) => Some(T::TimeTz),
        (T::Interval, T::Interval, BinaryOp::Add | BinaryOp::Subtract)
        | (T::Time, T::Time, BinaryOp::Subtract)
        | (T::TimeTz, T::TimeTz, BinaryOp::Subtract)
        | (
            T::Date | T::Timestamp | T::TimestampTz,
            T::Date | T::Timestamp | T::TimestampTz,
            BinaryOp::Subtract,
        ) => Some(T::Interval),
        (T::Interval, ty, BinaryOp::Multiply | BinaryOp::Divide) if numeric_rank(ty).is_some() => {
            Some(T::Interval)
        }
        (ty, T::Interval, BinaryOp::Multiply) if numeric_rank(ty).is_some() => Some(T::Interval),
        _ => None,
    }
}

fn binary_operator_name(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Equal => "=",
        BinaryOp::NotEqual => "<>",
        BinaryOp::Less => "<",
        BinaryOp::LessEqual => "<=",
        BinaryOp::Greater => ">",
        BinaryOp::GreaterEqual => ">=",
        BinaryOp::Add => "+",
        BinaryOp::Subtract => "-",
        BinaryOp::Multiply => "*",
        BinaryOp::Divide => "/",
    }
}
