//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite` persistence and connection affinity for common logical Key/Value sessions.

use std::{path::Path, sync::Arc};

use crate::{ManagedConnection, Result as SQLiteResult};
use uqa_storage::mvcc::{StorageTransactionId, VersionedKeyValueStore, VersionedSessionOptions};
use uqa_storage::read_control::{KeyValueReadVisitor, StorageReadControl, ValueReadVisitor};
use uqa_storage::{
    CatalogFacade, KeyValueBatch, KeyValueCatalog, KeyValueStorageBackend, KeyValueStore,
    PersistentStorageBackend, PersistentStorageIdentity, PersistentStorageProvider,
    PersistentStorageSession, StorageBackendResult,
};

/// A logical byte-store session sharing transaction state with its managed connection clones.
#[derive(Clone)]
pub struct SQLiteKeyValueStore {
    conn: ManagedConnection,
    records: Arc<VersionedKeyValueStore>,
}

impl SQLiteKeyValueStore {
    pub fn open(path: &Path) -> SQLiteResult<Self> {
        Self::new(ManagedConnection::open(path)?)
    }

    pub fn open_in_memory() -> SQLiteResult<Self> {
        Self::new(ManagedConnection::open_in_memory()?)
    }

    pub fn new(conn: ManagedConnection) -> SQLiteResult<Self> {
        Self::with_options(conn, VersionedSessionOptions::default())
    }

    /// Bind the connection and every existing clone to one bounded logical session.
    pub fn with_options(
        conn: ManagedConnection,
        options: VersionedSessionOptions,
    ) -> SQLiteResult<Self> {
        let records = conn.bind_records(options)?;
        Ok(Self { conn, records })
    }

    pub fn connection(&self) -> ManagedConnection {
        self.conn.clone()
    }

    pub fn new_session(&self) -> Self {
        let conn = self.conn.new_session();
        let records = conn
            .bind_records(self.records.options())
            .expect("a fresh connection inherits the same logical session configuration");
        Self { conn, records }
    }

    pub fn pending_commit(&self) -> Option<StorageTransactionId> {
        self.records.pending_commit()
    }
}

impl KeyValueStore for SQLiteKeyValueStore {
    fn with_read_view(
        &self,
        read: &mut uqa_storage::key_value::KeyValueReadScope<'_>,
    ) -> StorageBackendResult<()> {
        self.conn.with_records(|store| store.with_read_view(read))
    }

    fn with_mutation(
        &self,
        mutate: &mut uqa_storage::key_value::KeyValueMutation<'_>,
    ) -> StorageBackendResult<()> {
        self.conn.with_records(|store| store.with_mutation(mutate))
    }

    fn transaction_affinity(&self) -> Option<uqa_storage::StorageSessionAffinity> {
        Some(self.records.session_affinity())
    }

    fn storage_identity(&self) -> StorageBackendResult<Option<PersistentStorageIdentity>> {
        self.records.storage_identity()
    }

    fn open_session(&self) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
        Ok(Arc::new(self.new_session()))
    }

    fn get(&self, key: &[u8]) -> StorageBackendResult<Option<Vec<u8>>> {
        self.conn.with_records(|store| store.get(key))
    }

    fn contains_key(&self, key: &[u8]) -> StorageBackendResult<bool> {
        self.conn.with_records(|store| store.contains_key(key))
    }

    fn contains_prefix_budgeted(
        &self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        self.conn
            .with_records(|store| store.contains_prefix_budgeted(prefix, control))
    }

    fn visit_value(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.conn
            .with_records(|store| store.visit_value(key, control, visit))
    }

    fn visit_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.conn
            .with_records(|store| store.visit_prefix_after(prefix, after, limit, control, visit))
    }

    fn put(&self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.conn.with_records(|store| store.put(key, value))
    }

    fn delete(&self, key: &[u8]) -> StorageBackendResult<()> {
        self.conn.with_records(|store| store.delete(key))
    }

    fn delete_prefix(&self, prefix: &[u8]) -> StorageBackendResult<usize> {
        self.conn.with_records(|store| store.delete_prefix(prefix))
    }

    fn scan_prefix(&self, prefix: &[u8]) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.conn.with_records(|store| store.scan_prefix(prefix))
    }

    fn scan_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.conn
            .with_records(|store| store.scan_prefix_after(prefix, after, limit))
    }

    fn scan_prefix_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<Vec<Vec<u8>>> {
        self.conn
            .with_records(|store| store.scan_prefix_keys_after(prefix, after, limit))
    }

    fn first_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
    ) -> StorageBackendResult<Option<(Vec<u8>, Vec<u8>)>> {
        self.conn
            .with_records(|store| store.first_prefix_after(prefix, after))
    }

    fn batch(&self) -> Box<dyn KeyValueBatch + '_> {
        Box::new(SQLiteKeyValueBatch {
            connection: &self.conn,
            batch: self.records.batch(),
        })
    }

    fn begin_transaction(&self) -> StorageBackendResult<()> {
        self.conn.begin_transaction().map_err(Into::into)
    }

    fn begin_read_transaction(&self) -> StorageBackendResult<()> {
        self.conn.begin_record_read().map_err(Into::into)
    }

    fn begin_upgradeable_transaction(&self) -> StorageBackendResult<()> {
        self.conn.begin_deferred_transaction().map_err(Into::into)
    }

    fn in_transaction(&self) -> bool {
        self.conn.in_transaction()
    }

    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        self.conn.transaction_has_written().map_err(Into::into)
    }

    fn change_version(&self) -> StorageBackendResult<Option<u64>> {
        self.conn.data_version().map_err(Into::into)
    }

    fn change_version_monitor_is_nonblocking(&self) -> StorageBackendResult<bool> {
        Ok(true)
    }

    fn pin_transaction_snapshot(&self) -> StorageBackendResult<()> {
        self.conn.pin_transaction_snapshot().map_err(Into::into)
    }

    fn commit_transaction(&self) -> StorageBackendResult<()> {
        self.conn.commit_transaction().map_err(Into::into)
    }

    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        self.conn.rollback_transaction().map_err(Into::into)
    }

    fn savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.conn.savepoint(name).map_err(Into::into)
    }

    fn release_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.conn.release_savepoint(name).map_err(Into::into)
    }

    fn rollback_to_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.conn.rollback_to_savepoint(name).map_err(Into::into)
    }
}

struct SQLiteKeyValueBatch<'a> {
    connection: &'a ManagedConnection,
    batch: Box<dyn KeyValueBatch + 'a>,
}

impl KeyValueBatch for SQLiteKeyValueBatch<'_> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.batch.put(key, value)
    }
    fn delete(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.batch.delete(key)
    }
    fn delete_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.batch.delete_prefix(prefix)
    }
    fn replace_occurrence_record(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        self.batch.replace_occurrence_record(key, value)
    }
    fn invalidate_occurrence_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.batch.invalidate_occurrence_prefix(prefix)
    }
    fn occurrence_document(&mut self, table: &str, document: u64) -> StorageBackendResult<()> {
        self.batch.occurrence_document(table, document)
    }
    fn reset_occurrences(&mut self, table: &str) -> StorageBackendResult<()> {
        self.batch.reset_occurrences(table)
    }
    fn fence_record(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.batch.fence_record(key)
    }
    fn graph_mutation(
        &mut self,
        mutation: uqa_storage::mvcc::GraphMutation<'_>,
    ) -> StorageBackendResult<()> {
        self.batch.graph_mutation(mutation)
    }
    fn preview_graph_invalidation(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        self.batch.preview_graph_invalidation(key, value)
    }
    fn replace_graph_cache(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        self.batch.replace_graph_cache(key, value)
    }
    fn commit(self: Box<Self>) -> StorageBackendResult<()> {
        let Self { connection, batch } = *self;
        connection.with_records(|_| batch.commit())
    }
}

#[cfg(test)]
#[path = "key_value/controlled/tests.rs"]
mod controlled_tests;

/// Shared `SQLite` `KeyValue` storage handle with catalog and backend factories.
#[derive(Clone)]
pub struct SQLiteKeyValueStorage {
    store: Arc<SQLiteKeyValueStore>,
}

pub type SQLiteKeyValueCatalog = KeyValueCatalog;
pub type SQLiteKeyValueStorageBackend = KeyValueStorageBackend;

impl SQLiteKeyValueStorage {
    pub fn open(path: &Path) -> SQLiteResult<Self> {
        Self::open_with_options(path, VersionedSessionOptions::default())
    }

    pub fn open_with_options(path: &Path, options: VersionedSessionOptions) -> SQLiteResult<Self> {
        Self::from_connection_with_options(ManagedConnection::open(path)?, options)
    }

    pub fn open_in_memory() -> SQLiteResult<Self> {
        Ok(Self {
            store: Arc::new(SQLiteKeyValueStore::open_in_memory()?),
        })
    }

    pub fn from_connection(conn: ManagedConnection) -> SQLiteResult<Self> {
        Self::from_connection_with_options(conn, VersionedSessionOptions::default())
    }

    pub fn from_connection_with_options(
        conn: ManagedConnection,
        options: VersionedSessionOptions,
    ) -> SQLiteResult<Self> {
        Ok(Self {
            store: Arc::new(SQLiteKeyValueStore::with_options(conn, options)?),
        })
    }

    pub fn store(&self) -> Arc<SQLiteKeyValueStore> {
        Arc::clone(&self.store)
    }

    pub fn catalog(&self) -> SQLiteKeyValueCatalog {
        let store: Arc<dyn KeyValueStore> = self.store.clone();
        KeyValueCatalog::new(store)
    }

    pub fn backend(&self) -> SQLiteKeyValueStorageBackend {
        let store: Arc<dyn KeyValueStore> = self.store.clone();
        KeyValueStorageBackend::new(store)
    }
}

impl PersistentStorageProvider for SQLiteKeyValueStorage {
    fn open_session(&self) -> StorageBackendResult<PersistentStorageSession> {
        let store: Arc<dyn KeyValueStore> = Arc::new(self.store.new_session());
        let catalog: Arc<dyn CatalogFacade> = Arc::new(KeyValueCatalog::new(Arc::clone(&store)));
        let backend: Arc<dyn PersistentStorageBackend> =
            Arc::new(KeyValueStorageBackend::new(store));
        Ok(PersistentStorageSession::new(catalog, backend))
    }

    fn storage_identity(
        &self,
    ) -> StorageBackendResult<Option<uqa_storage::PersistentStorageIdentity>> {
        let connection = self.store.connection();
        let Some(path) = connection.database_path() else {
            return Ok(None);
        };
        uqa_storage::PersistentStorageIdentity::for_database_path(path)
            .map(Some)
            .map_err(|error| {
                uqa_storage::StorageBackendError::Other(format!(
                    "resolve SQLite key/value database identity `{}`: {error}",
                    path.display()
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use uqa_analysis::standard_analyzer;
    use uqa_core::Value;
    use uqa_storage::catalog::{ColumnStatsInput, TableSchema};
    use uqa_storage::{
        CatalogFacade, PersistentStorageBackend, VectorIndexOpenMode, VectorIndexSpec,
    };

    use super::*;

    #[test]
    fn sqlite_key_value_store_round_trips_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keyvalue.sqlite3");
        {
            let store = SQLiteKeyValueStore::open(&path).unwrap();
            store.put(b"apple/1", b"red").unwrap();
            store.put(b"apple/2", b"green").unwrap();
            store.put(b"banana/1", b"yellow").unwrap();
            store.put(&[0x10, 0xff, 0x01], b"binary-prefix").unwrap();
            store.put(&[0x11, 0x00], b"binary-neighbour").unwrap();
            assert_eq!(store.get(b"apple/1").unwrap().as_deref(), Some(&b"red"[..]));
            assert_eq!(store.scan_prefix(b"apple/").unwrap().len(), 2);
            assert_eq!(
                store
                    .scan_prefix_keys_after(b"apple/", Some(b"apple/1"), 1)
                    .unwrap(),
                vec![b"apple/2".to_vec()]
            );
            assert_eq!(
                store
                    .scan_prefix_keys_after(b"apple/", Some(b"a"), 2)
                    .unwrap(),
                vec![b"apple/1".to_vec(), b"apple/2".to_vec()]
            );
            assert!(store
                .scan_prefix_keys_after(b"apple/", Some(b"z"), 2)
                .unwrap()
                .is_empty());
            assert_eq!(
                store.first_prefix_after(b"apple/", Some(b"a")).unwrap(),
                Some((b"apple/1".to_vec(), b"red".to_vec()))
            );
            assert!(store
                .scan_prefix_keys_after(b"apple/", None, 0)
                .unwrap()
                .is_empty());
            assert_eq!(store.scan_prefix(&[0x10, 0xff]).unwrap().len(), 1);
            store.delete_prefix(&[0x10, 0xff]).unwrap();
            assert_eq!(
                store.get(&[0x11, 0x00]).unwrap().as_deref(),
                Some(&b"binary-neighbour"[..])
            );
        }
        {
            let store = SQLiteKeyValueStore::open(&path).unwrap();
            assert_eq!(
                store.get(b"apple/2").unwrap().as_deref(),
                Some(&b"green"[..])
            );
            store.delete_prefix(b"apple/").unwrap();
            assert!(store.get(b"apple/1").unwrap().is_none());
            assert_eq!(
                store.get(b"banana/1").unwrap().as_deref(),
                Some(&b"yellow"[..])
            );
        }
    }

    #[test]
    fn sqlite_store_passes_the_reusable_backend_contract() {
        let directory = tempfile::tempdir().unwrap();
        let reader =
            SQLiteKeyValueStore::open(&directory.path().join("conformance.sqlite3")).unwrap();
        let writer = reader.new_session();
        uqa_storage::key_value::conformance::verify_store(&reader).unwrap();
        uqa_storage::key_value::conformance::verify_session_isolation(&reader, &writer).unwrap();
    }

    #[test]
    fn sqlite_key_value_batch_is_atomic() {
        let store = SQLiteKeyValueStore::open_in_memory().unwrap();
        let mut batch = store.batch();
        batch.put(b"k1", b"v1").unwrap();
        batch.put(b"k2", b"v2").unwrap();
        batch.commit().unwrap();
        assert_eq!(store.get(b"k1").unwrap().as_deref(), Some(&b"v1"[..]));

        store.begin_transaction().unwrap();
        store.put(b"k3", b"v3").unwrap();
        store.rollback_transaction().unwrap();
        assert!(store.get(b"k3").unwrap().is_none());
    }

    #[test]
    fn sqlite_key_value_storage_supports_existing_store_contracts() {
        let storage = SQLiteKeyValueStorage::open_in_memory().unwrap();
        let backend = storage.backend();

        let mut docs = backend.document_store("articles");
        docs.put(
            1,
            BTreeMap::from([("title".to_string(), Value::Str("rust search".into()))]),
        )
        .unwrap();
        assert_eq!(
            docs.get_field(1, "title").unwrap(),
            Some(Value::Str("rust search".into()))
        );

        let mut index = backend.inverted_index("articles", standard_analyzer("english"));
        index
            .add_document(1, BTreeMap::from([("title".into(), "rust search".into())]))
            .unwrap();
        assert_eq!(index.doc_freq("title", "rust").unwrap(), 1);

        let mut vectors = backend
            .vector_index(
                "articles",
                "embedding",
                2,
                VectorIndexSpec::BruteForce,
                VectorIndexOpenMode::Create,
            )
            .unwrap();
        vectors.add(1, vec![1.0, 0.0]).unwrap();
        vectors.add(2, vec![0.0, 1.0]).unwrap();
        let hits = vectors.search_knn(&[1.0, 0.0], 1).unwrap();
        assert_eq!(hits.entries()[0].doc_id, 1);
    }

    #[test]
    fn sqlite_key_value_catalog_supports_existing_registry_contracts() {
        let storage = SQLiteKeyValueStorage::open_in_memory().unwrap();
        let catalog = storage.catalog();
        catalog.set_metadata("schema_version", "keyvalue").unwrap();
        catalog.save_schema("public").unwrap();
        catalog
            .save_table(&TableSchema {
                relation: uqa_storage::RelationIdentity::new("public", "docs"),
                role_owner: "uqa".into(),
                acl: None,
                column_acls: std::collections::BTreeMap::default(),
                object_id: [1; 16],
                storage_generation: [1; 16],
                analyzer_json: "{}".into(),
                fts_fields: vec!["title".into()],
                vector_fields: Vec::new(),
                columns_json: "[]".into(),
                constraints_json: String::new(),
            })
            .unwrap();
        catalog
            .save_analyzer("ko", "{\"name\":\"standard\"}")
            .unwrap();
        catalog
            .save_table_field_analyzer("docs", "title", "index", "ko")
            .unwrap();
        catalog
            .save_foreign_server("fs", "memory", "{\"root\":\"/tmp\"}")
            .unwrap();
        catalog
            .save_catalog_index(
                &uqa_storage::RelationIdentity::new("public", "idx_docs_title"),
                "gin",
                "public.docs",
                "[\"title\"]",
                "{}",
            )
            .unwrap();
        catalog
            .save_column_stats(ColumnStatsInput::basic(
                "docs",
                "title",
                4,
                0,
                Some("a"),
                Some("z"),
                10,
            ))
            .unwrap();

        assert_eq!(
            catalog.get_metadata("schema_version").unwrap().as_deref(),
            Some("keyvalue")
        );
        assert_eq!(
            catalog.load_tables().unwrap()[0].relation.qualified_name(),
            "public.docs"
        );
        assert_eq!(catalog.load_analyzers().unwrap()[0].0, "ko");
        assert_eq!(catalog.load_foreign_servers().unwrap()[0].0, "fs");
        assert_eq!(
            catalog.load_catalog_indexes().unwrap()[0]
                .relation
                .qualified_name(),
            "public.idx_docs_title"
        );
        assert_eq!(
            catalog.load_column_stats("docs").unwrap()[0].distinct_count,
            4
        );
    }
}
