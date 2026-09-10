//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Common-context value coercion after SQL type binding.

use crate::{ColumnType, SQLError};
use uqa_core::Value;

pub fn coerce_common_context_value(
    value: Value,
    source_type: Option<&ColumnType>,
    target_type: Option<&ColumnType>,
) -> Result<Value, SQLError> {
    let Some(target_type) = target_type else {
        return Ok(value);
    };
    if source_type == Some(target_type) {
        return Ok(value);
    }
    let cast_target = match target_type {
        ColumnType::Domain { base, .. } => base.as_ref(),
        target => target,
    };
    let source_name = source_type.map(ColumnType::sql_name);
    crate::expr::cast_value_from(&value, &cast_target.sql_name(), source_name.as_deref())
}
