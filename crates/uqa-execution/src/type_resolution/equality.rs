//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` equality-operator operand binding.

use uqa_sql::ast::ColumnType;
use uqa_sql::SQLError;

use super::{
    canonical_column_type_name, common::base_type, common_type, require_equality_operator,
    routine_type_accepts_implicit_cast,
};

pub fn equality_operand_type(
    left: &ColumnType,
    right: &ColumnType,
) -> Result<ColumnType, SQLError> {
    require_equality_operator(left).map_err(|_| undefined_equality_operator(left, right))?;
    require_equality_operator(right).map_err(|_| undefined_equality_operator(left, right))?;
    if matches!(left, ColumnType::Json) || matches!(right, ColumnType::Json) {
        return Err(undefined_equality_operator(left, right));
    }
    if matches!(left, ColumnType::Array(_)) || matches!(right, ColumnType::Array(_)) {
        if left == right {
            return Ok(left.clone());
        }
        return Err(undefined_equality_operator(left, right));
    }
    if left.is_character_string() && right.is_character_string() {
        if matches!(left, ColumnType::Bpchar | ColumnType::Character(_))
            || matches!(right, ColumnType::Bpchar | ColumnType::Character(_))
        {
            return Ok(ColumnType::Bpchar);
        }
        return Ok(ColumnType::Text);
    }
    common_type(left, right).map_err(|_| undefined_equality_operator(left, right))
}

/// Resolve the comparison type accepted by a `PostgreSQL` foreign key. The referenced key's operator class makes compatibility directional: an implicit cast from the referencing type to the referenced type is valid, while the reverse cast alone is not. Integer widths, floating widths, character strings, and date/timestamp types additionally have cross-type equality operators in their shared operator families.
pub fn foreign_key_operand_type(
    referencing: &ColumnType,
    referenced: &ColumnType,
) -> Result<ColumnType, SQLError> {
    let referencing = base_type(referencing);
    let referenced = base_type(referenced);
    let comparison = equality_operand_type(referencing, referenced)?;
    let referencing_name = canonical_column_type_name(referencing);
    let referenced_name = canonical_column_type_name(referenced);
    let shared_operator_family = is_integral(referencing) && is_integral(referenced)
        || is_float(referencing) && is_float(referenced)
        || is_date_timestamp(referencing) && is_date_timestamp(referenced)
        || referencing.is_character_string() && referenced.is_character_string();
    if referencing == referenced
        || routine_type_accepts_implicit_cast(&referencing_name, &referenced_name)
        || shared_operator_family
    {
        return Ok(comparison);
    }
    Err(undefined_equality_operator(referencing, referenced))
}

fn is_integral(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::SmallInteger | ColumnType::Integer | ColumnType::BigInteger
    )
}

fn is_float(ty: &ColumnType) -> bool {
    matches!(ty, ColumnType::Real | ColumnType::DoublePrecision)
}

fn is_date_timestamp(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Date | ColumnType::Timestamp | ColumnType::TimestampTz
    )
}

fn undefined_equality_operator(left: &ColumnType, right: &ColumnType) -> SQLError {
    SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!(
            "operator does not exist: {} = {}",
            left.sql_name(),
            right.sql_name()
        ),
    }
}
