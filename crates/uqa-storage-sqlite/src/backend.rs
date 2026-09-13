//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session factories and physical stores for the `SQLite` backend.

use crate::{
    Catalog, ManagedConnection, SQLiteBTreeIndexStore, SQLiteDocumentStore, SQLiteHNSWIndex,
    SQLiteIVFIndex, SQLiteInvertedIndex, SQLiteVectorIndex,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_analysis::Analyzer;
use uqa_core::{DocId, Value};
use uqa_storage::{
    CatalogFacade, DocumentStore, InvertedIndex, PersistentStorageBackend,
    PersistentStorageIdentity, PersistentStorageProvider, PersistentStorageSession,
    StorageBackendError, StorageBackendResult, StorageSavepointId, VectorIndex,
    VectorIndexOpenMode, VectorIndexSpec,
};

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
    fn open_initial_session(&self) -> StorageBackendResult<PersistentStorageSession> {
        let connection = self.connection.new_session();
        let catalog: Arc<dyn CatalogFacade> =
            Arc::new(Catalog::for_initial_restore(connection.clone()));
        let backend: Arc<dyn PersistentStorageBackend> =
            Arc::new(SQLiteStorageBackend::new(connection));
        Ok(PersistentStorageSession::new(catalog, backend))
    }

    fn open_session(&self) -> StorageBackendResult<PersistentStorageSession> {
        let connection = self.connection.new_session();
        let catalog: Arc<dyn CatalogFacade> = Arc::new(Catalog::open(connection.clone())?);
        let backend: Arc<dyn PersistentStorageBackend> =
            Arc::new(SQLiteStorageBackend::new(connection));
        Ok(PersistentStorageSession::new(catalog, backend))
    }

    fn storage_identity(&self) -> StorageBackendResult<Option<PersistentStorageIdentity>> {
        let Some(path) = self.connection.database_path() else {
            return Ok(None);
        };
        PersistentStorageIdentity::for_database_path(path)
            .map(Some)
            .map_err(|error| {
                StorageBackendError::Other(format!(
                    "resolve SQLite database identity `{}`: {error}",
                    path.display()
                ))
            })
    }
}

impl PersistentStorageBackend for SQLiteStorageBackend {
    fn storage_identity(&self) -> StorageBackendResult<Option<PersistentStorageIdentity>> {
        let Some(path) = self.conn.database_path() else {
            return Ok(None);
        };
        PersistentStorageIdentity::for_database_path(path).map(Some)
    }

    fn open_session(&self) -> StorageBackendResult<PersistentStorageSession> {
        let connection = self.conn.new_session();
        let catalog: Arc<dyn CatalogFacade> = Arc::new(Catalog::open(connection.clone())?);
        let backend: Arc<dyn PersistentStorageBackend> = Arc::new(Self::new(connection));
        Ok(PersistentStorageSession::new(catalog, backend))
    }

    fn supports_concurrent_pinned_read_and_write(&self) -> bool {
        self.conn.supports_concurrent_pinned_read_and_write()
    }

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
        field: &uqa_storage::ValueIndexKey,
    ) -> StorageBackendResult<Option<Vec<(DocId, Value)>>> {
        Ok(SQLiteBTreeIndexStore::new(self.conn.clone()).load(table, field)?)
    }

    fn btree_index_fields(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<uqa_storage::ValueIndexKey>> {
        Ok(SQLiteBTreeIndexStore::new(self.conn.clone()).fields(table)?)
    }

    fn btree_index_repairs(
        &self,
    ) -> StorageBackendResult<Vec<(String, uqa_storage::ValueIndexKey)>> {
        Ok(SQLiteBTreeIndexStore::new(self.conn.clone()).repairs()?)
    }

    fn clear_btree_index_repair(
        &self,
        table: &str,
        field: &uqa_storage::ValueIndexKey,
    ) -> StorageBackendResult<()> {
        SQLiteBTreeIndexStore::new(self.conn.clone()).clear_repair(table, field)?;
        Ok(())
    }

    fn replace_btree_index(
        &self,
        table: &str,
        field: &uqa_storage::ValueIndexKey,
        values: &[(DocId, Value)],
    ) -> StorageBackendResult<()> {
        SQLiteBTreeIndexStore::new(self.conn.clone()).replace(table, field, values)?;
        Ok(())
    }

    fn repair_btree_index(
        &self,
        table: &str,
        field: &uqa_storage::ValueIndexKey,
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
        indexes: &[(&uqa_storage::ValueIndexKey, &[(DocId, Value)])],
    ) -> StorageBackendResult<()> {
        SQLiteBTreeIndexStore::new(self.conn.clone()).replace_many(table, indexes)?;
        Ok(())
    }

    fn apply_btree_index_write(
        &self,
        table: &str,
        doc_id: DocId,
        values: Option<&BTreeMap<uqa_storage::ValueIndexKey, Value>>,
    ) -> StorageBackendResult<()> {
        SQLiteBTreeIndexStore::new(self.conn.clone()).apply_write(table, doc_id, values)?;
        Ok(())
    }

    fn drop_btree_index(
        &self,
        table: &str,
        field: &uqa_storage::ValueIndexKey,
    ) -> StorageBackendResult<()> {
        SQLiteBTreeIndexStore::new(self.conn.clone()).drop_index(table, field)?;
        Ok(())
    }

    fn clear_btree_indexes(&self, table: &str) -> StorageBackendResult<()> {
        SQLiteBTreeIndexStore::new(self.conn.clone()).clear_table(table)?;
        Ok(())
    }

    fn vacuum(&self) -> StorageBackendResult<()> {
        self.conn.vacuum()?;
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

    fn begin_upgradeable_transaction(&self) -> StorageBackendResult<()> {
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

    fn savepoint(&self, id: StorageSavepointId) -> StorageBackendResult<()> {
        self.conn.savepoint(&id.backend_name())?;
        Ok(())
    }

    fn release_savepoint(&self, id: StorageSavepointId) -> StorageBackendResult<()> {
        self.conn.release_savepoint(&id.backend_name())?;
        Ok(())
    }

    fn rollback_to_savepoint(&self, id: StorageSavepointId) -> StorageBackendResult<()> {
        self.conn.rollback_to_savepoint(&id.backend_name())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use uqa_analysis::analyzer::standard_analyzer;
    use uqa_core::Value;

    use super::*;
    use crate::{Catalog, SQLiteError};

    #[test]
    fn session_factory_reads_current_catalog_while_a_sibling_holds_a_writer_reservation() {
        let directory = tempfile::tempdir().unwrap();
        let connection = ManagedConnection::open_compressed(
            &directory.path().join("session-writer-reservation.db"),
            crate::SQLiteCompressionOptions::default(),
        )
        .unwrap();
        let provider = SQLiteStorageProvider::new(connection);
        let writer = provider.open_session().unwrap();
        writer.backend.begin_transaction().unwrap();
        writer
            .catalog
            .set_metadata("private-write", "uncommitted")
            .unwrap();

        let reader = provider.open_session().unwrap();
        assert_eq!(reader.catalog.get_metadata("private-write").unwrap(), None);
        writer.backend.commit_transaction().unwrap();
        assert_eq!(
            reader
                .catalog
                .get_metadata("private-write")
                .unwrap()
                .as_deref(),
            Some("uncommitted")
        );
    }

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
                VectorIndexSpec::IVF(uqa_storage::IVFIndexParams {
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
            Err(StorageBackendError::Backend { source, .. })
                if matches!(source.downcast_ref::<SQLiteError>(), Some(SQLiteError::TransactionAborted(_)))
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
