//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::Expr;

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<IndexCatalogIdentity>,
    #[serde(default, skip_serializing_if = "IndexRelationships::is_empty")]
    pub relationships: IndexRelationships,
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

/// The index address and physical namespace survive SQL name changes. Legacy conversion retains the original physical key without rebuilding postings or evaluating stored expressions.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexCatalogIdentity {
    pub identity: uqa_core::catalog_identity::CatalogObjectIdentity,
    pub table_object_id: [u8; 16],
    pub physical_key: String,
}

pub mod identity;

mod relationships;
pub use relationships::{can_attach_index, IndexAttachmentShape, IndexRelationships};

mod enforced_key;
pub use enforced_key::{referenceable_keys, EnforcedKey};

pub mod stored;
