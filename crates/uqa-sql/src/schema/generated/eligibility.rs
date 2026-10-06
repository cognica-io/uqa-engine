//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` diagnostics shared by generated-column validation.

use crate::SQLError;

pub(crate) fn non_immutable_function() -> SQLError {
    SQLError::Routine {
        sqlstate: "42P17".into(),
        message: "generation expression is not immutable".into(),
    }
}
