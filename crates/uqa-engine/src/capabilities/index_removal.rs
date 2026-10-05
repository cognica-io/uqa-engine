//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind index removal to catalog generations, session locks and provider publication.
use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_execution::schema::indexes::removal::{
    IndexRemovalCatalog, IndexRemovalContext, IndexRemovalPrivileges, IndexRemovalPublication,
    IndexRemovalTransactions, IndexRemovalWrite,
};
use uqa_sql::{ast::ColumnType, catalog::resolution::RelationResolution, SQLError, SQLResult};
use uqa_storage::{CatalogIndexRow, StorageBackendResult};
impl Engine {
    pub(crate) fn index_removal_context(&self) -> IndexRemovalContext<'_> {
        IndexRemovalContext {
            deletion: self,
            catalog: self,
            privileges: self,
            publication: self,
            constraints: self.constraint_alter_context(),
            transactions: self,
            notices: self.query_runtime_view().notices,
        }
    }
}
impl IndexRemovalCatalog for Engine {
    fn resolve_relation_kind(&self, name: &str) -> Result<RelationResolution, SQLError> {
        self.resolve_visible_relation_kind(name)
    }
    fn bound_catalog_index(&self, name: &str) -> StorageBackendResult<Option<CatalogIndexRow>> {
        Engine::bound_catalog_index(self, name)
    }
    fn list_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        Engine::catalog_indexes_in_execution(self)
    }
    fn column_type(&self, table: &str, column: &str) -> StorageBackendResult<Option<ColumnType>> {
        Engine::column_type(self, table, column)
    }
}
impl IndexRemovalPrivileges for Engine {
    fn ensure_drop_authority(&self, index: &CatalogIndexRow) -> Result<(), SQLError> {
        self.require_index_drop_authority(index)
    }
}
impl IndexRemovalPublication for Engine {
    fn retire_diskann_index(
        &self,
        row: &CatalogIndexRow,
        field: &str,
        dimensions: u32,
    ) -> StorageBackendResult<()> {
        self.retire_catalog_diskann_index(row, field, dimensions)
    }
    fn drop_catalog_index_relation(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<Option<CatalogIndexRow>> {
        self.try_drop_catalog_index_relation(relation)
    }
    fn drop_fts_field(&self, table: &str, field: &str) -> Result<(), SQLError> {
        Engine::drop_fts_field(self, table, field)
    }
    fn release_fts_analyzer_owner(&self, table: &str, field: &str) -> Result<(), SQLError> {
        Engine::release_fts_analyzer_owner(self, table, field)
    }
    fn drop_vector_field_index(
        &self,
        table: &str,
        field: String,
        dimensions: u32,
    ) -> StorageBackendResult<bool> {
        Engine::drop_vector_field_index(self, table, field, dimensions)
    }
    fn drop_vector_index_metadata(&self, table: &str, field: &str) -> StorageBackendResult<()> {
        Engine::drop_vector_index_metadata(self, table, field)
    }
}
impl IndexRemovalTransactions for Engine {
    fn with_index_write(&self, write: IndexRemovalWrite<'_>) -> Result<SQLResult, SQLError> {
        self.with_implicit_transaction(|engine| write(&engine.index_removal_context()))
    }
}
