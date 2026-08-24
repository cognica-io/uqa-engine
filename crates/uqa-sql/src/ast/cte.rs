//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Common-table-expression syntax and recursive traversal controls.

use serde::{Deserialize, Serialize};

use super::{Expr, SelectStmt};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CTE {
    pub name: String,
    pub columns: Vec<String>,
    pub recursive: bool,
    #[serde(default)]
    pub materialization: CteMaterialization,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<CteSearchClause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle: Option<CteCycleClause>,
    pub query: Box<SelectStmt>,
}

/// The planning fence requested for one common-table expression.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CteMaterialization {
    #[default]
    Default,
    Materialized,
    NotMaterialized,
}

/// `PostgreSQL` recursive-CTE traversal-order metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CteSearchClause {
    pub columns: Vec<String>,
    pub breadth_first: bool,
    pub sequence_column: String,
}

/// `PostgreSQL` recursive-CTE cycle detection metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CteCycleClause {
    pub columns: Vec<String>,
    pub mark_column: String,
    pub mark_value: Expr,
    pub mark_default: Expr,
    pub path_column: String,
}
