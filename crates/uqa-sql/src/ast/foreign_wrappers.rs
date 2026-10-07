//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Written foreign-wrapper declarations, before catalog lookup and validation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateForeignWrapper {
    pub name: String,
    /// Preserve order and repeated clauses for execution-time `PostgreSQL` diagnostics.
    pub functions: Vec<ForeignWrapperFunctionOption>,
    pub options: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForeignWrapperFunctionOption {
    Handler(Option<Vec<String>>),
    Validator(Option<Vec<String>>),
}
