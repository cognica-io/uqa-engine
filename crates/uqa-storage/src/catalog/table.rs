//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

pub use uqa_core::catalog_acl::{TableAclEntry, TablePrivileges};

use super::{RelationIdentity, RelationSecurityRow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub relation: RelationIdentity,
    #[serde(flatten)]
    pub security: RelationSecurityRow,
    /// Stable logical relation identity. `CREATE TABLE` allocates a new value, while renames, schema changes, `TRUNCATE`, and reopen preserve it. A zero value marks a legacy catalog row that the engine upgrades during open.
    #[serde(default)]
    pub object_id: [u8; 16],
    /// Stable physical-storage identity. `CREATE TABLE` and `TRUNCATE` allocate a new value, while schema-only alterations and renames preserve it. A zero value marks a legacy catalog row that the engine upgrades during open.
    #[serde(default)]
    pub storage_generation: [u8; 16],
    pub analyzer_json: String,
    pub fts_fields: Vec<String>,
    pub vector_fields: Vec<VectorFieldSchema>,
    /// Serialized `Vec<uqa_sql::ast::ColumnDef>` capturing the schema
    /// columns (name, type, `auto_increment`, flags). Empty for
    /// tables created by the legacy code path before column tracking
    /// existed.
    #[serde(default)]
    pub columns_json: String,
    /// Serialized `uqa_sql::ast::TableConstraintSet`. Empty for catalogs
    /// created before durable table constraints were introduced.
    #[serde(default)]
    pub constraints_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorFieldSchema {
    pub field: String,
    pub dimensions: u32,
}
