//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` equality-operator operand binding.

use uqa_sql::ast::ColumnType;
use uqa_sql::SQLError;

use super::common_type;

pub fn equality_operand_type(
    left: &ColumnType,
    right: &ColumnType,
) -> Result<ColumnType, SQLError> {
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
