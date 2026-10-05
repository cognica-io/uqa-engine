//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained schema generations and catalog publication for stored dependency rewrites.
use super::TableSchemaState;
use crate::catalog::foreign::StoredForeignTable;
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_storage::StorageBackendResult;

pub type LoadedTableSchema<'a> = (String, Box<dyn TableSchemaState + 'a>);
pub trait LoadedTableSchemas {
    fn table_schemas(&self) -> Vec<LoadedTableSchema<'_>>;
}
pub trait ForeignSchemaPublication {
    fn foreign_tables(&self) -> BTreeMap<RelationIdentity, StoredForeignTable>;
    fn persist_foreign_table(
        &self,
        relation: &RelationIdentity,
        table: &StoredForeignTable,
    ) -> StorageBackendResult<()>;
    fn publish_foreign_tables(&self, updates: Vec<(RelationIdentity, StoredForeignTable)>);
}
pub trait CatalogPublicationChanges {
    fn prepared_catalog_changed(
        &self,
        _change: crate::statement::prepared::invalidation::PreparedCatalogChange,
    ) {
    }
    /// Publish a change to an existing, already-bound relation. The session adapter lends its loaded object identity; this does not resolve a SQL name.
    fn prepared_relation_changed(&self, _relation: &RelationIdentity) {}
    fn table_catalog_changed(&self);
    fn catalog_registry_changed(&self);
}
pub struct SchemaDependencyPublicationContext<'a> {
    pub tables: &'a dyn LoadedTableSchemas,
    pub foreign: &'a dyn ForeignSchemaPublication,
    pub changes: &'a dyn CatalogPublicationChanges,
}
