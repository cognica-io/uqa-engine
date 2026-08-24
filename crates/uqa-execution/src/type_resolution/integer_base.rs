//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Internal execution names selected by the common built-in overload binder.

use uqa_sql::expr::{
    TO_BIN_INT4_FUNCTION, TO_BIN_INT8_FUNCTION, TO_HEX_INT4_FUNCTION, TO_HEX_INT8_FUNCTION,
    TO_OCT_INT4_FUNCTION, TO_OCT_INT8_FUNCTION,
};

pub(super) fn is_bound_function(name: &str) -> bool {
    matches!(
        name,
        TO_BIN_INT4_FUNCTION
            | TO_BIN_INT8_FUNCTION
            | TO_HEX_INT4_FUNCTION
            | TO_HEX_INT8_FUNCTION
            | TO_OCT_INT4_FUNCTION
            | TO_OCT_INT8_FUNCTION
    )
}
