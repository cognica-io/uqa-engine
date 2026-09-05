//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent-storage facade that assembles the key/value adapters.

use super::{
    btree_index, Analyzer, Arc, BTreeMap, DocId, DocumentStore, InvertedIndex, KeyValueCatalog,
    KeyValueDocumentStore, KeyValueHNSWIndex, KeyValueIVFIndex, KeyValueInvertedIndex,
    KeyValueStore, KeyValueVectorIndex, PersistentStorageBackend, PersistentStorageIdentity,
    StorageBackendResult, Value, VectorIndex, VectorIndexOpenMode, VectorIndexSpec,
};
use crate::{CatalogFacade, PersistentStorageSession};

/// Persistent storage factory implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueStorageBackend {
    store: Arc<dyn KeyValueStore>,
}

impl KeyValueStorageBackend {
    pub fn new(store: Arc<dyn KeyValueStore>) -> Self {
        Self { store }
    }

    pub fn store(&self) -> Arc<dyn KeyValueStore> {
        Arc::clone(&self.store)
    }
}

impl PersistentStorageBackend for KeyValueStorageBackend {
    fn storage_identity(&self) -> StorageBackendResult<Option<PersistentStorageIdentity>> {
        self.store.storage_identity()
    }

    fn open_session(&self) -> StorageBackendResult<PersistentStorageSession> {
        let store = self.store.open_session()?;
        let catalog: Arc<dyn CatalogFacade> = Arc::new(KeyValueCatalog::new(Arc::clone(&store)));
        let backend: Arc<dyn PersistentStorageBackend> = Arc::new(Self::new(store));
        Ok(PersistentStorageSession::new(catalog, backend))
    }

    fn document_store(&self, table: &str) -> Box<dyn DocumentStore> {
        Box::new(KeyValueDocumentStore::new(Arc::clone(&self.store), table))
    }

    fn migrate_document_storage(&self) -> StorageBackendResult<()> {
        KeyValueDocumentStore::migrate_legacy_storage(self.store.as_ref())
    }

    fn inverted_index(&self, table: &str, analyzer: Analyzer) -> Box<dyn InvertedIndex> {
        Box::new(KeyValueInvertedIndex::new(
            Arc::clone(&self.store),
            table,
            analyzer,
        ))
    }

    fn migrate_inverted_index_storage(&self) -> StorageBackendResult<()> {
        KeyValueInvertedIndex::migrate_legacy_storage(self.store.as_ref())
    }

    fn vector_index(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        spec: VectorIndexSpec,
        mode: VectorIndexOpenMode,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        match spec {
            VectorIndexSpec::BruteForce => Ok(Box::new(KeyValueVectorIndex::new(
                Arc::clone(&self.store),
                table,
                field,
                dimensions,
            ))),
            VectorIndexSpec::IVF(params) => match mode {
                VectorIndexOpenMode::Create => Ok(Box::new(KeyValueIVFIndex::create(
                    Arc::clone(&self.store),
                    table,
                    field,
                    dimensions,
                    params,
                )?)),
                VectorIndexOpenMode::Restore => Ok(Box::new(KeyValueIVFIndex::restore(
                    Arc::clone(&self.store),
                    table,
                    field,
                    dimensions,
                    params,
                )?)),
            },
            VectorIndexSpec::HNSW(params) => match mode {
                VectorIndexOpenMode::Create => Ok(Box::new(KeyValueHNSWIndex::create(
                    Arc::clone(&self.store),
                    table,
                    field,
                    dimensions,
                    params,
                )?)),
                VectorIndexOpenMode::Restore => Ok(Box::new(KeyValueHNSWIndex::restore(
                    Arc::clone(&self.store),
                    table,
                    field,
                    dimensions,
                    params,
                )?)),
            },
        }
    }

    fn drop_vector_index_metadata(&self, table: &str, field: &str) -> StorageBackendResult<()> {
        KeyValueIVFIndex::drop_metadata(self.store.as_ref(), table, field)
    }

    fn persists_btree_indexes(&self) -> bool {
        true
    }

    fn load_btree_index(
        &self,
        table: &str,
        field: &crate::ValueIndexKey,
    ) -> StorageBackendResult<Option<Vec<(DocId, Value)>>> {
        btree_index::load(self.store.as_ref(), table, field)
    }

    fn btree_index_fields(&self, table: &str) -> StorageBackendResult<Vec<crate::ValueIndexKey>> {
        btree_index::fields(self.store.as_ref(), table)
    }

    fn replace_btree_index(
        &self,
        table: &str,
        field: &crate::ValueIndexKey,
        values: &[(DocId, Value)],
    ) -> StorageBackendResult<()> {
        btree_index::replace(self.store.as_ref(), table, field, values)
    }

    fn replace_btree_indexes(
        &self,
        table: &str,
        indexes: &[(&crate::ValueIndexKey, &[(DocId, Value)])],
    ) -> StorageBackendResult<()> {
        btree_index::replace_many(self.store.as_ref(), table, indexes)
    }

    fn apply_btree_index_write(
        &self,
        table: &str,
        doc_id: DocId,
        values: Option<&BTreeMap<crate::ValueIndexKey, Value>>,
    ) -> StorageBackendResult<()> {
        btree_index::apply_write(self.store.as_ref(), table, doc_id, values)
    }

    fn drop_btree_index(
        &self,
        table: &str,
        field: &crate::ValueIndexKey,
    ) -> StorageBackendResult<()> {
        btree_index::drop_index(self.store.as_ref(), table, field)
    }

    fn clear_btree_indexes(&self, table: &str) -> StorageBackendResult<()> {
        btree_index::clear_entries(self.store.as_ref(), table)
    }

    fn begin_transaction(&self) -> StorageBackendResult<()> {
        self.store.begin_transaction()
    }

    fn begin_read_transaction(&self) -> StorageBackendResult<()> {
        self.store.begin_read_transaction()
    }

    fn begin_upgradeable_transaction(&self) -> StorageBackendResult<()> {
        self.store.begin_upgradeable_transaction()
    }

    fn in_transaction(&self) -> bool {
        self.store.in_transaction()
    }

    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        self.store.transaction_has_written()
    }

    fn change_version(&self) -> StorageBackendResult<Option<u64>> {
        self.store.change_version()
    }

    fn change_version_monitor_is_nonblocking(&self) -> StorageBackendResult<bool> {
        self.store.change_version_monitor_is_nonblocking()
    }

    fn pin_transaction_snapshot(&self) -> StorageBackendResult<()> {
        self.store.pin_transaction_snapshot()
    }

    fn commit_transaction(&self) -> StorageBackendResult<()> {
        self.store.commit_transaction()
    }

    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        self.store.rollback_transaction()
    }

    fn savepoint(&self, id: crate::StorageSavepointId) -> StorageBackendResult<()> {
        self.store.savepoint(&id.backend_name())
    }

    fn release_savepoint(&self, id: crate::StorageSavepointId) -> StorageBackendResult<()> {
        self.store.release_savepoint(&id.backend_name())
    }

    fn rollback_to_savepoint(&self, id: crate::StorageSavepointId) -> StorageBackendResult<()> {
        self.store.rollback_to_savepoint(&id.backend_name())
    }
}
