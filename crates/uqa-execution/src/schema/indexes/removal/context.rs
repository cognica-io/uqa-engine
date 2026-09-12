//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog reads, physical publication and transaction inputs used by index removal.
use crate::schema::constraints::ConstraintAlterContext;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{ColumnType, ForeignKey},
    catalog::resolution::RelationResolution,
    SQLError, SQLResult,
};
use uqa_storage::{CatalogIndexRow, StorageBackendResult};
pub trait IndexRemovalCatalog {
    fn resolve_relation_kind(&self, name: &str) -> Result<RelationResolution, SQLError>;
    fn bound_catalog_index(&self, name: &str) -> StorageBackendResult<Option<CatalogIndexRow>>;
    fn has_constraint_index(&self, relation: &RelationIdentity) -> bool;
    fn list_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>>;
    fn column_type(&self, table: &str, column: &str) -> StorageBackendResult<Option<ColumnType>>;
}
pub trait IndexRemovalPrivileges {
    fn ensure_drop_authority(&self, index: &CatalogIndexRow) -> Result<(), SQLError>;
}
pub trait IndexRemovalReferrers {
    fn referrers_to(&self, table: &str) -> StorageBackendResult<Vec<(String, ForeignKey)>>;
}
pub trait IndexRemovalLocks {
    fn lock_exclusive(&self, table: &str) -> Result<(), SQLError>;
}
pub trait IndexRemovalPublication {
    fn drop_catalog_index_relation(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<Option<CatalogIndexRow>>;
    fn drop_fts_field(&self, table: &str, field: &str) -> Result<(), String>;
    fn release_fts_analyzer_owner(&self, table: &str, field: &str) -> Result<(), String>;
    fn drop_vector_field_index(
        &self,
        table: &str,
        field: String,
        dimensions: u32,
    ) -> StorageBackendResult<bool>;
    fn drop_vector_index_metadata(&self, table: &str, field: &str) -> StorageBackendResult<()>;
}
pub type IndexRemovalWrite<'a> =
    Box<dyn FnOnce(&IndexRemovalContext<'_>) -> Result<SQLResult, SQLError> + 'a>;
pub trait IndexRemovalTransactions {
    fn with_index_write(&self, write: IndexRemovalWrite<'_>) -> Result<SQLResult, SQLError>;
}
pub struct IndexRemovalContext<'a> {
    pub catalog: &'a dyn IndexRemovalCatalog,
    pub privileges: &'a dyn IndexRemovalPrivileges,
    pub referrers: &'a dyn IndexRemovalReferrers,
    pub locks: &'a dyn IndexRemovalLocks,
    pub publication: &'a dyn IndexRemovalPublication,
    pub constraints: ConstraintAlterContext<'a>,
    pub transactions: &'a dyn IndexRemovalTransactions,
    pub notices: &'a parking_lot::Mutex<Vec<(String, String)>>,
}
