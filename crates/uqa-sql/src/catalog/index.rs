//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::Expr;

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexDefinition {
    #[serde(default)]
    pub included_columns: Vec<String>,
    #[serde(default)]
    pub column_order: Vec<crate::ast::IndexColumnOrder>,
    #[serde(default)]
    pub key_names: Vec<String>,
    #[serde(default)]
    pub key_types: Vec<crate::ast::ColumnType>,
    #[serde(default)]
    pub predicate: Option<Box<Expr>>,
    pub unique: bool,
    pub nulls_not_distinct: bool,
}

mod enforced_key;
pub use enforced_key::EnforcedKey;

pub mod stored;
