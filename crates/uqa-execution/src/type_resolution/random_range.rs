//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Internal execution names selected by the common built-in overload binder.

use uqa_sql::ast::ColumnType;
use uqa_sql::expr::{RANDOM_INT4_FUNCTION, RANDOM_INT8_FUNCTION, RANDOM_NUMERIC_FUNCTION};

pub(super) fn bound_function_type(name: &str) -> Option<ColumnType> {
    Some(match name {
        RANDOM_INT4_FUNCTION => ColumnType::Integer,
        RANDOM_INT8_FUNCTION => ColumnType::BigInteger,
        RANDOM_NUMERIC_FUNCTION => ColumnType::Numeric {
            precision: None,
            scale: None,
        },
        _ => return None,
    })
}
