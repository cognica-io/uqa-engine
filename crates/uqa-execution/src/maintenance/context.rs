//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained relation state, physical publication and session boundaries used by VACUUM.
use crate::mutation::publication::DocumentVectors;
use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};
use uqa_core::DocId;
use uqa_sql::{
    ast::RelationPersistence,
    maintenance::{VacuumCatalog, VacuumPrivileges},
    SQLError,
};
use uqa_storage::{
    document_store::Document, DocumentStore, StorageBackendResult, StoredDocument, VectorIndex,
};
/// Saved statistics for the retained relation; maintenance chooses when to restore and persist them.
pub trait VacuumStatistics {
    fn loaded(&self) -> bool;
    fn dirty(&self) -> bool;
    fn restore(&self);
    fn persist(&self, table: &str) -> StorageBackendResult<()>;
}
pub type VacuumDocuments<'a> = Box<dyn Deref<Target = Box<dyn DocumentStore>> + 'a>;
pub type VacuumVectorIndexes<'a> =
    Box<dyn DerefMut<Target = BTreeMap<String, Box<dyn VectorIndex>>> + 'a>;
pub trait VacuumTable {
    fn statistics(&self) -> Box<dyn VacuumStatistics + '_>;
    fn documents(&self) -> VacuumDocuments<'_>;
    fn document_vectors(&self, document: &Document) -> Result<DocumentVectors, SQLError>;
    fn clear_documents(&self) -> StorageBackendResult<()>;
    fn clear_text_index(&self) -> StorageBackendResult<()>;
    fn vector_indexes(&self) -> VacuumVectorIndexes<'_>;
    fn clear_value_indexes(&self);
    fn persistence(&self) -> RelationPersistence;
    fn mark_doc_count_dirty(&self);
}
pub trait VacuumRelations {
    fn require_table(&self, name: &str) -> Result<Box<dyn VacuumTable + '_>, SQLError>;
    fn scan_tables(&self, name: &str, descendants: bool) -> Result<Vec<String>, SQLError>;
}
pub trait VacuumLocks {
    fn lock_exclusive(&self, table: &str) -> Result<(), SQLError>;
    fn release_session(&self);
}
pub trait VacuumStorage {
    fn vacuum(&self) -> StorageBackendResult<()>;
    fn clear_btree_indexes(&self, table: &str) -> StorageBackendResult<()>;
}
pub trait VacuumRows {
    fn restore_document(
        &self,
        table: &str,
        id: DocId,
        document: StoredDocument,
        vectors: DocumentVectors,
    ) -> Result<(), SQLError>;
    fn refresh_indexes(&self, table: &str) -> StorageBackendResult<()>;
    fn note_table_data_changed(&self);
}
pub trait VacuumStatisticsRefresh {
    fn table_names(&self, operation: &str) -> Result<Vec<String>, SQLError>;
    fn analyze_target(
        &self,
        table: &str,
        columns: &[String],
        descendants: bool,
    ) -> StorageBackendResult<()>;
}
pub type VacuumWrite<'a> = Box<dyn FnOnce(&VacuumContext<'_>) -> StorageBackendResult<()> + 'a>;
pub trait VacuumTransactions {
    fn depth(&self) -> usize;
    fn with_maintenance_write(&self, write: VacuumWrite<'_>) -> StorageBackendResult<()>;
}
pub struct VacuumContext<'a> {
    pub catalog: &'a dyn VacuumCatalog,
    pub privileges: &'a dyn VacuumPrivileges,
    pub relations: &'a dyn VacuumRelations,
    pub locks: &'a dyn VacuumLocks,
    pub storage: &'a dyn VacuumStorage,
    pub rows: &'a dyn VacuumRows,
    pub statistics: &'a dyn VacuumStatisticsRefresh,
    pub transactions: &'a dyn VacuumTransactions,
}
