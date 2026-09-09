//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain declarations retain type identity and constraints until catalog binding.

use serde::{Deserialize, Serialize};

use super::{ColumnType, Expr};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDomain {
    pub name: String,
    pub base: ColumnType,
    pub collation: Option<String>,
    pub default: Option<Expr>,
    pub not_null: Option<DomainNotNull>,
    pub checks: Vec<DomainCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainNotNull {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainCheck {
    pub name: Option<String>,
    pub expression: Expr,
}
