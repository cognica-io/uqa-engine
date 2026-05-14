//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent storage backend factory.
//!
//! This boundary keeps the engine from constructing SQLite-backed stores
//! directly. Alternative persistent backends can implement the same factory
//! without changing query execution code.

use uqa_analysis::Analyzer;

use crate::document_store::DocumentStore;
use crate::inverted_index::InvertedIndex;
use crate::sqlite::{
    ManagedConnection, SQLiteDocumentStore, SQLiteError, SQLiteIVFIndex, SQLiteInvertedIndex,
};
use crate::vector_index::VectorIndex;

#[derive(Debug, thiserror::Error)]
pub enum StorageBackendError {
    #[error(transparent)]
    SQLite(#[from] SQLiteError),
    #[error("{0}")]
    Other(String),
}

pub type StorageBackendResult<T> = std::result::Result<T, StorageBackendError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistentVectorIndexParams {
    pub nlist: usize,
    pub nprobe: usize,
    pub train_threshold: usize,
}

impl Default for PersistentVectorIndexParams {
    fn default() -> Self {
        Self {
            nlist: 100,
            nprobe: 10,
            train_threshold: 256,
        }
    }
}

/// Factory plus transaction surface for persistent table/index storage.
pub trait PersistentStorageBackend: Send + Sync {
    fn document_store(&self, table: &str) -> Box<dyn DocumentStore>;

    fn inverted_index(&self, table: &str, analyzer: Analyzer) -> Box<dyn InvertedIndex>;

    fn vector_index(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        params: Option<PersistentVectorIndexParams>,
    ) -> Box<dyn VectorIndex>;

    fn begin_transaction(&self) -> StorageBackendResult<()>;

    fn commit_transaction(&self) -> StorageBackendResult<()>;

    fn rollback_transaction(&self) -> StorageBackendResult<()>;

    fn savepoint(&self, name: &str) -> StorageBackendResult<()>;

    fn release_savepoint(&self, name: &str) -> StorageBackendResult<()>;

    fn rollback_to_savepoint(&self, name: &str) -> StorageBackendResult<()>;
}

#[derive(Clone)]
pub struct SQLiteStorageBackend {
    conn: ManagedConnection,
}

impl SQLiteStorageBackend {
    pub fn new(conn: ManagedConnection) -> Self {
        Self { conn }
    }

    pub fn connection(&self) -> ManagedConnection {
        self.conn.clone()
    }
}

impl PersistentStorageBackend for SQLiteStorageBackend {
    fn document_store(&self, table: &str) -> Box<dyn DocumentStore> {
        Box::new(SQLiteDocumentStore::new(self.conn.clone(), table))
    }

    fn inverted_index(&self, table: &str, analyzer: Analyzer) -> Box<dyn InvertedIndex> {
        Box::new(SQLiteInvertedIndex::new(self.conn.clone(), table, analyzer))
    }

    fn vector_index(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        params: Option<PersistentVectorIndexParams>,
    ) -> Box<dyn VectorIndex> {
        match params {
            Some(params) => Box::new(SQLiteIVFIndex::with_params(
                self.conn.clone(),
                table,
                field,
                dimensions,
                params.nlist,
                params.nprobe,
                params.train_threshold,
            )),
            None => Box::new(SQLiteIVFIndex::new(
                self.conn.clone(),
                table,
                field,
                dimensions,
            )),
        }
    }

    fn begin_transaction(&self) -> StorageBackendResult<()> {
        self.conn.begin_transaction()?;
        Ok(())
    }

    fn commit_transaction(&self) -> StorageBackendResult<()> {
        self.conn.commit_transaction()?;
        Ok(())
    }

    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        self.conn.rollback_transaction()?;
        Ok(())
    }

    fn savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.conn.savepoint(name)?;
        Ok(())
    }

    fn release_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.conn.release_savepoint(name)?;
        Ok(())
    }

    fn rollback_to_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.conn.rollback_to_savepoint(name)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use uqa_analysis::analyzer::standard_analyzer;
    use uqa_core::Value;

    use super::*;
    use crate::sqlite::Catalog;

    #[test]
    fn sqlite_backend_builds_document_index_and_vector_stores() {
        let conn = ManagedConnection::open_in_memory().unwrap();
        let _catalog = Catalog::open(conn.clone()).unwrap();
        let backend = SQLiteStorageBackend::new(conn);

        let mut doc = BTreeMap::new();
        doc.insert("title".to_string(), Value::Str("rust storage".into()));
        let mut docs = backend.document_store("articles");
        docs.put(1, doc);
        assert_eq!(
            docs.get_field(1, "title"),
            Some(Value::Str("rust storage".into()))
        );

        let mut inv = backend.inverted_index("articles", standard_analyzer("english"));
        inv.add_document(
            1,
            BTreeMap::from([("title".to_string(), "rust storage".to_string())]),
        );
        assert_eq!(inv.doc_freq("title", "rust"), 1);

        let mut vectors = backend.vector_index(
            "articles",
            "embedding",
            2,
            Some(PersistentVectorIndexParams {
                nlist: 2,
                nprobe: 1,
                train_threshold: 2,
            }),
        );
        vectors.add(1, vec![1.0, 0.0]);
        let hits = vectors.search_knn(&[1.0, 0.0], 1);
        assert_eq!(hits.entries().len(), 1);
        assert_eq!(hits.entries()[0].doc_id, 1);
    }

    #[test]
    fn sqlite_backend_transaction_rolls_back_cross_store_writes() {
        let conn = ManagedConnection::open_in_memory().unwrap();
        let _catalog = Catalog::open(conn.clone()).unwrap();
        let backend = SQLiteStorageBackend::new(conn);
        let mut docs = backend.document_store("articles");
        let mut inv = backend.inverted_index("articles", standard_analyzer("english"));

        backend.begin_transaction().unwrap();
        docs.put(
            1,
            BTreeMap::from([("title".to_string(), Value::Str("rollback".into()))]),
        );
        inv.add_document(
            1,
            BTreeMap::from([("title".to_string(), "rollback".to_string())]),
        );
        backend.rollback_transaction().unwrap();

        assert_eq!(docs.len(), 0);
        assert_eq!(inv.doc_freq("title", "rollback"), 0);
    }
}
