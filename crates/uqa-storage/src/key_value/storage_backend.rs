//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent-storage facade that assembles the key/value adapters.

use super::{
    Analyzer, Arc, DocumentStore, InvertedIndex, KeyValueDocumentStore, KeyValueInvertedIndex,
    KeyValueStore, KeyValueVectorIndex, PersistentStorageBackend, StorageBackendError,
    StorageBackendResult, VectorIndex, VectorIndexOpenMode, VectorIndexSpec,
};

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
    fn document_store(&self, table: &str) -> Box<dyn DocumentStore> {
        Box::new(KeyValueDocumentStore::new(Arc::clone(&self.store), table))
    }

    fn inverted_index(&self, table: &str, analyzer: Analyzer) -> Box<dyn InvertedIndex> {
        Box::new(KeyValueInvertedIndex::new(
            Arc::clone(&self.store),
            table,
            analyzer,
        ))
    }

    fn vector_index(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        spec: VectorIndexSpec,
        _mode: VectorIndexOpenMode,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        match spec {
            VectorIndexSpec::BruteForce => Ok(Box::new(KeyValueVectorIndex::new(
                Arc::clone(&self.store),
                table,
                field,
                dimensions,
            ))),
            other => Err(StorageBackendError::Other(format!(
                "{} vector indexes are not supported by the key/value backend",
                other.access_method()
            ))),
        }
    }

    fn begin_transaction(&self) -> StorageBackendResult<()> {
        self.store.begin_transaction()
    }

    fn commit_transaction(&self) -> StorageBackendResult<()> {
        self.store.commit_transaction()
    }

    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        self.store.rollback_transaction()
    }

    fn savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.store.savepoint(name)
    }

    fn release_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.store.release_savepoint(name)
    }

    fn rollback_to_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.store.rollback_to_savepoint(name)
    }
}
