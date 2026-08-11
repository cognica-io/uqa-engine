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

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_analysis::Analyzer;
use uqa_core::{DocId, Value};

use crate::document_store::DocumentStore;
use crate::inverted_index::InvertedIndex;
use crate::sqlite::{
    Catalog, ManagedConnection, SQLiteBTreeIndexStore, SQLiteDocumentStore, SQLiteError,
    SQLiteHNSWIndex, SQLiteIVFIndex, SQLiteInvertedIndex, SQLiteVectorIndex,
};
use crate::vector_index::{VectorIndex, VectorIndexOpenMode, VectorIndexSpec};
use crate::CatalogFacade;

#[derive(Debug, thiserror::Error)]
pub enum StorageBackendError {
    #[error("text analysis failed: {0}")]
    Analysis(#[from] uqa_analysis::AnalysisError),
    #[error(transparent)]
    SQLite(#[from] SQLiteError),
    #[error("payload serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("{backend} storage failed: {source}")]
    Backend {
        backend: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("{0}")]
    Other(String),
}

impl StorageBackendError {
    pub fn backend(
        backend: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Backend {
            backend,
            source: Box::new(source),
        }
    }
}

pub type StorageBackendResult<T> = std::result::Result<T, StorageBackendError>;

/// Session-bound catalog and physical storage handles created together.
///
/// Both handles must share the same transaction context. Keeping their
/// construction behind one provider prevents a catalog write from escaping
/// through a different connection or transaction than document/index writes.
pub struct PersistentStorageSession {
    pub catalog: Arc<dyn CatalogFacade>,
    pub backend: Arc<dyn PersistentStorageBackend>,
}

impl PersistentStorageSession {
    pub fn new(
        catalog: Arc<dyn CatalogFacade>,
        backend: Arc<dyn PersistentStorageBackend>,
    ) -> Self {
        Self { catalog, backend }
    }
}

/// Factory for independent sessions over one durable database.
///
/// A provider owns the database-level resource while each returned session
/// owns its transaction state. This is the engine-facing extension point for
/// `SQLite`, redb, and application-defined Key/Value stores.
pub trait PersistentStorageProvider: Send + Sync {
    fn open_session(&self) -> StorageBackendResult<PersistentStorageSession>;
}

/// Factory plus transaction surface for persistent table/index storage.
pub trait PersistentStorageBackend: Send + Sync {
    fn document_store(&self, table: &str) -> Box<dyn DocumentStore>;

    fn inverted_index(&self, table: &str, analyzer: Analyzer) -> Box<dyn InvertedIndex>;

    /// Upgrade backend-owned inverted-index values before table handles are
    /// restored. Implementations must make the rewrite atomic and idempotent.
    fn migrate_inverted_index_storage(&self) -> StorageBackendResult<()> {
        Ok(())
    }

    fn vector_index(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        spec: VectorIndexSpec,
        mode: VectorIndexOpenMode,
    ) -> StorageBackendResult<Box<dyn VectorIndex>>;

    fn drop_vector_index_metadata(&self, _table: &str, _field: &str) -> StorageBackendResult<()> {
        Ok(())
    }

    /// Whether this backend maps logical `btree` indexes to durable postings.
    fn persists_btree_indexes(&self) -> bool {
        false
    }

    /// `Some(entries)` is a complete persisted index; `None` means it has not
    /// been built yet and the engine must backfill it from documents once.
    fn load_btree_index(
        &self,
        _table: &str,
        _field: &str,
    ) -> StorageBackendResult<Option<Vec<(DocId, Value)>>> {
        Ok(None)
    }

    fn btree_index_fields(&self, _table: &str) -> StorageBackendResult<Vec<String>> {
        Ok(Vec::new())
    }

    /// Fields whose persisted posting support was found inconsistent during a
    /// schema migration. The engine repairs these at its explicit open-time
    /// write boundary and clears each durable retry marker only after success.
    fn btree_index_repairs(&self) -> StorageBackendResult<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    fn clear_btree_index_repair(&self, _table: &str, _field: &str) -> StorageBackendResult<()> {
        Ok(())
    }

    fn replace_btree_index(
        &self,
        _table: &str,
        _field: &str,
        _values: &[(DocId, Value)],
    ) -> StorageBackendResult<()> {
        Ok(())
    }

    /// Repair sparse support differences without requiring capable backends
    /// to rewrite every already-valid posting. The complete replacement is
    /// supplied for the storage-neutral fallback.
    fn repair_btree_index(
        &self,
        table: &str,
        field: &str,
        complete: &[(DocId, Value)],
        _stale_doc_ids: &[DocId],
        _missing: &[(DocId, Value)],
    ) -> StorageBackendResult<()> {
        self.replace_btree_index(table, field, complete)
    }

    /// Replace several complete indexes for one table atomically. Backends
    /// may override this to share one transaction and prepared statements.
    fn replace_btree_indexes(
        &self,
        table: &str,
        indexes: &[(&str, &[(DocId, Value)])],
    ) -> StorageBackendResult<()> {
        for (field, values) in indexes {
            self.replace_btree_index(table, field, values)?;
        }
        Ok(())
    }

    fn apply_btree_index_write(
        &self,
        _table: &str,
        _doc_id: DocId,
        _values: Option<&BTreeMap<String, Value>>,
    ) -> StorageBackendResult<()> {
        Ok(())
    }

    fn drop_btree_index(&self, _table: &str, _field: &str) -> StorageBackendResult<()> {
        Ok(())
    }

    fn clear_btree_indexes(&self, _table: &str) -> StorageBackendResult<()> {
        Ok(())
    }

    fn begin_transaction(&self) -> StorageBackendResult<()>;

    /// Begin a transaction whose first operation is expected to be a read.
    /// Backends with distinct lock modes may defer write-lock acquisition;
    /// the default preserves existing transaction semantics.
    fn begin_read_transaction(&self) -> StorageBackendResult<()> {
        self.begin_transaction()
    }

    /// Whether this session currently owns a pinned storage transaction.
    fn in_transaction(&self) -> bool;

    /// Whether the current transaction has performed a physical write.
    ///
    /// The engine uses this to enforce read-only statement transactions even
    /// for writes made through catalog/index helpers it did not classify.
    fn transaction_has_written(&self) -> StorageBackendResult<bool>;

    /// Backend commit generation visible to this session, when available.
    /// A changing value invalidates session-local catalog and index caches.
    fn change_version(&self) -> StorageBackendResult<Option<u64>> {
        Ok(None)
    }

    /// Whether reading [`Self::change_version`] can proceed while this
    /// session owns its write transaction.
    fn change_version_monitor_is_nonblocking(&self) -> StorageBackendResult<bool> {
        Ok(true)
    }

    /// Pin the transaction's read snapshot before cache restoration.
    fn pin_transaction_snapshot(&self) -> StorageBackendResult<()> {
        Ok(())
    }

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

    /// Create a backend whose stores use an independent transaction session
    /// over the same physical `SQLite` pool.
    #[must_use]
    pub fn new_session(&self) -> Self {
        Self::new(self.conn.new_session())
    }
}

/// Database-level owner that creates isolated `SQLite` engine sessions.
#[derive(Clone)]
pub struct SQLiteStorageProvider {
    connection: ManagedConnection,
}

impl SQLiteStorageProvider {
    pub fn new(connection: ManagedConnection) -> Self {
        Self { connection }
    }
}

impl PersistentStorageProvider for SQLiteStorageProvider {
    fn open_session(&self) -> StorageBackendResult<PersistentStorageSession> {
        let connection = self.connection.new_session();
        let catalog: Arc<dyn CatalogFacade> = Arc::new(Catalog::open(connection.clone())?);
        let backend: Arc<dyn PersistentStorageBackend> =
            Arc::new(SQLiteStorageBackend::new(connection));
        Ok(PersistentStorageSession::new(catalog, backend))
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
        spec: VectorIndexSpec,
        mode: VectorIndexOpenMode,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        let index: Box<dyn VectorIndex> = match spec {
            VectorIndexSpec::BruteForce => Box::new(SQLiteVectorIndex::new(
                self.conn.clone(),
                table,
                field,
                dimensions,
            )),
            VectorIndexSpec::IVF(params) => {
                params.validate()?;
                match mode {
                    VectorIndexOpenMode::Create => Box::new(SQLiteIVFIndex::with_params(
                        self.conn.clone(),
                        table,
                        field,
                        dimensions,
                        params.nlist,
                        params.nprobe,
                        params.train_threshold,
                    )),
                    VectorIndexOpenMode::Restore => Box::new(SQLiteIVFIndex::open_existing(
                        self.conn.clone(),
                        table,
                        field,
                        dimensions,
                        params.nlist,
                        params.nprobe,
                        params.train_threshold,
                    )),
                }
            }
            VectorIndexSpec::HNSW(params) => {
                params.validate()?;
                match mode {
                    VectorIndexOpenMode::Create => Box::new(SQLiteHNSWIndex::with_params(
                        self.conn.clone(),
                        table,
                        field,
                        dimensions,
                        params,
                    )),
                    VectorIndexOpenMode::Restore => {
                        let index = SQLiteHNSWIndex::open_existing(
                            self.conn.clone(),
                            table,
                            field,
                            dimensions,
                            params,
                        );
                        index.validate_existing()?;
                        Box::new(index)
                    }
                }
            }
        };
        Ok(index)
    }

    fn drop_vector_index_metadata(&self, table: &str, field: &str) -> StorageBackendResult<()> {
        SQLiteIVFIndex::drop_metadata(&self.conn, table, field)?;
        SQLiteHNSWIndex::drop_metadata(&self.conn, table, field)?;
        Ok(())
    }

    fn persists_btree_indexes(&self) -> bool {
        true
    }

    fn load_btree_index(
        &self,
        table: &str,
        field: &str,
    ) -> StorageBackendResult<Option<Vec<(DocId, Value)>>> {
        Ok(SQLiteBTreeIndexStore::new(self.conn.clone()).load(table, field)?)
    }

    fn btree_index_fields(&self, table: &str) -> StorageBackendResult<Vec<String>> {
        Ok(SQLiteBTreeIndexStore::new(self.conn.clone()).fields(table)?)
    }

    fn btree_index_repairs(&self) -> StorageBackendResult<Vec<(String, String)>> {
        Ok(SQLiteBTreeIndexStore::new(self.conn.clone()).repairs()?)
    }

    fn clear_btree_index_repair(&self, table: &str, field: &str) -> StorageBackendResult<()> {
        SQLiteBTreeIndexStore::new(self.conn.clone()).clear_repair(table, field)?;
        Ok(())
    }

    fn replace_btree_index(
        &self,
        table: &str,
        field: &str,
        values: &[(DocId, Value)],
    ) -> StorageBackendResult<()> {
        SQLiteBTreeIndexStore::new(self.conn.clone()).replace(table, field, values)?;
        Ok(())
    }

    fn repair_btree_index(
        &self,
        table: &str,
        field: &str,
        _complete: &[(DocId, Value)],
        stale_doc_ids: &[DocId],
        missing: &[(DocId, Value)],
    ) -> StorageBackendResult<()> {
        SQLiteBTreeIndexStore::new(self.conn.clone()).repair(
            table,
            field,
            stale_doc_ids,
            missing,
        )?;
        Ok(())
    }

    fn replace_btree_indexes(
        &self,
        table: &str,
        indexes: &[(&str, &[(DocId, Value)])],
    ) -> StorageBackendResult<()> {
        SQLiteBTreeIndexStore::new(self.conn.clone()).replace_many(table, indexes)?;
        Ok(())
    }

    fn apply_btree_index_write(
        &self,
        table: &str,
        doc_id: DocId,
        values: Option<&BTreeMap<String, Value>>,
    ) -> StorageBackendResult<()> {
        SQLiteBTreeIndexStore::new(self.conn.clone()).apply_write(table, doc_id, values)?;
        Ok(())
    }

    fn drop_btree_index(&self, table: &str, field: &str) -> StorageBackendResult<()> {
        SQLiteBTreeIndexStore::new(self.conn.clone()).drop_index(table, field)?;
        Ok(())
    }

    fn clear_btree_indexes(&self, table: &str) -> StorageBackendResult<()> {
        SQLiteBTreeIndexStore::new(self.conn.clone()).clear_table(table)?;
        Ok(())
    }

    fn begin_transaction(&self) -> StorageBackendResult<()> {
        self.conn.begin_transaction()?;
        Ok(())
    }

    fn begin_read_transaction(&self) -> StorageBackendResult<()> {
        self.conn.begin_deferred_transaction()?;
        Ok(())
    }

    fn in_transaction(&self) -> bool {
        self.conn.in_transaction()
    }

    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        Ok(self.conn.transaction_has_written()?)
    }

    fn change_version(&self) -> StorageBackendResult<Option<u64>> {
        Ok(self.conn.data_version()?)
    }

    fn change_version_monitor_is_nonblocking(&self) -> StorageBackendResult<bool> {
        Ok(self.conn.data_version_monitor_is_nonblocking()?)
    }

    fn pin_transaction_snapshot(&self) -> StorageBackendResult<()> {
        self.conn.pin_transaction_snapshot()?;
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
        docs.put(1, doc).unwrap();
        assert_eq!(
            docs.get_field(1, "title").unwrap(),
            Some(Value::Str("rust storage".into()))
        );

        let mut inv = backend.inverted_index("articles", standard_analyzer("english"));
        inv.add_document(
            1,
            BTreeMap::from([("title".to_string(), "rust storage".to_string())]),
        )
        .unwrap();
        assert_eq!(inv.doc_freq("title", "rust").unwrap(), 1);

        let mut vectors = backend
            .vector_index(
                "articles",
                "embedding",
                2,
                VectorIndexSpec::IVF(crate::IVFIndexParams {
                    nlist: 2,
                    nprobe: 1,
                    train_threshold: 2,
                }),
                VectorIndexOpenMode::Create,
            )
            .unwrap();
        vectors.add(1, vec![1.0, 0.0]).unwrap();
        let hits = vectors.search_knn(&[1.0, 0.0], 1).unwrap();
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
        )
        .unwrap();
        inv.add_document(
            1,
            BTreeMap::from([("title".to_string(), "rollback".to_string())]),
        )
        .unwrap();
        backend.rollback_transaction().unwrap();

        assert_eq!(docs.len().unwrap(), 0);
        assert_eq!(inv.doc_freq("title", "rollback").unwrap(), 0);
    }

    #[test]
    fn sqlite_sessions_isolate_and_atomically_commit_cross_store_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cross-store-isolation.sqlite3");
        let conn = ManagedConnection::open(&path).unwrap();
        let catalog = Catalog::open(conn.clone()).unwrap();
        let writer = SQLiteStorageBackend::new(conn.clone());
        let observer_conn = conn.new_session();
        let observer_catalog = Catalog::open(observer_conn.clone()).unwrap();
        let observer = SQLiteStorageBackend::new(observer_conn);

        let mut writer_docs = writer.document_store("articles");
        let mut writer_inv = writer.inverted_index("articles", standard_analyzer("english"));
        let mut writer_vectors = writer
            .vector_index(
                "articles",
                "embedding",
                2,
                VectorIndexSpec::BruteForce,
                VectorIndexOpenMode::Create,
            )
            .unwrap();
        let observer_docs = observer.document_store("articles");
        let observer_inv = observer.inverted_index("articles", standard_analyzer("english"));
        let observer_vectors = observer
            .vector_index(
                "articles",
                "embedding",
                2,
                VectorIndexSpec::BruteForce,
                VectorIndexOpenMode::Restore,
            )
            .unwrap();

        writer.begin_transaction().unwrap();
        writer_docs
            .put(
                1,
                BTreeMap::from([("title".to_string(), Value::Str("atomic rust".into()))]),
            )
            .unwrap();
        writer_inv
            .add_document(
                1,
                BTreeMap::from([("title".to_string(), "atomic rust".to_string())]),
            )
            .unwrap();
        writer_vectors.add(1, vec![1.0, 0.0]).unwrap();
        catalog
            .save_scoring_params("transactional", r#"{"alpha":1.0}"#)
            .unwrap();

        assert_eq!(writer_docs.len().unwrap(), 1);
        assert_eq!(writer_inv.doc_freq("title", "rust").unwrap(), 1);
        assert_eq!(writer_vectors.count().unwrap(), 1);
        assert!(catalog
            .load_scoring_params("transactional")
            .unwrap()
            .is_some());

        assert_eq!(observer_docs.len().unwrap(), 0);
        assert_eq!(observer_inv.doc_freq("title", "rust").unwrap(), 0);
        assert_eq!(observer_vectors.count().unwrap(), 0);
        assert!(observer_catalog
            .load_scoring_params("transactional")
            .unwrap()
            .is_none());

        writer.commit_transaction().unwrap();
        assert_eq!(observer_docs.len().unwrap(), 1);
        assert_eq!(observer_inv.doc_freq("title", "rust").unwrap(), 1);
        assert_eq!(observer_vectors.count().unwrap(), 1);
        assert!(observer_catalog
            .load_scoring_params("transactional")
            .unwrap()
            .is_some());
    }

    #[test]
    fn ignored_legacy_index_error_cannot_commit_partial_document_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ignored-index-error.sqlite3");
        let conn = ManagedConnection::open(&path).unwrap();
        let _catalog = Catalog::open(conn.clone()).unwrap();
        let backend = SQLiteStorageBackend::new(conn.clone());
        let observer = conn.new_session();
        let mut docs = backend.document_store("articles");
        let mut vectors = backend
            .vector_index(
                "articles",
                "embedding",
                2,
                VectorIndexSpec::BruteForce,
                VectorIndexOpenMode::Create,
            )
            .unwrap();

        backend.begin_transaction().unwrap();
        docs.put(
            1,
            BTreeMap::from([("title".to_string(), Value::Str("must roll back".into()))]),
        )
        .unwrap();
        conn.with(|connection| {
            connection.execute("DROP TABLE _vectors", [])?;
            Ok(())
        })
        .unwrap();
        // The vector write reports its error directly. Even if a caller
        // ignores that Result, the managed transaction is poisoned and the
        // partial document write cannot commit.
        let ignored = vectors.add(1, vec![1.0, 0.0]);
        assert!(ignored.is_err());
        assert!(matches!(
            backend.commit_transaction(),
            Err(StorageBackendError::SQLite(
                SQLiteError::TransactionAborted(_)
            ))
        ));

        let stored_docs: i64 = observer
            .with(|connection| {
                Ok(connection.query_row(
                    "SELECT COUNT(*) FROM _documents WHERE table_name = 'articles'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        let vector_table_exists: i64 = observer
            .with(|connection| {
                Ok(connection.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_vectors'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(stored_docs, 0);
        assert_eq!(vector_table_exists, 1);
    }
}
