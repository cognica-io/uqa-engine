//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable index catalog metadata shared by storage and planning.

use crate::RelationIdentity;

/// One row from the secondary-index registry.
#[derive(Debug, Clone)]
pub struct CatalogIndexRow {
    pub relation: RelationIdentity,
    pub index_type: String,
    pub table_name: String,
    pub columns_json: String,
    pub parameters_json: String,
    /// Durable semantic definition; absent on legacy ordinary secondary indexes.
    pub definition_json: Option<String>,
}
