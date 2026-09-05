//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite`-backed physical `KeyValue` storage.
//!
//! The logical catalog, document store, inverted index, and vector index live
//! in `uqa-storage::key_value`. This crate provides the `SQLite` implementation
//! of the ordered byte-key store they require.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use uqa_storage::key_value::{
    prefix_upper_bound, KeyValueBatch, KeyValueCatalog, KeyValueStorageBackend, KeyValueStore,
};
use uqa_storage::sqlite::{ManagedConnection, Result as SQLiteResult, SQLiteError};
use uqa_storage::{
    CatalogFacade, PersistentStorageBackend, PersistentStorageIdentity, PersistentStorageProvider,
    PersistentStorageSession, StorageBackendError, StorageBackendResult,
};

const KEY_VALUE_TABLE: &str = "_key_value";

#[derive(Debug, Clone)]
enum SQLiteKeyValueBatchOperation {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
    DeletePrefix(Vec<u8>),
}

/// `SQLite` physical implementation of [`KeyValueStore`].
#[derive(Clone)]
pub struct SQLiteKeyValueStore {
    conn: ManagedConnection,
    table_ready: Arc<AtomicBool>,
}

impl SQLiteKeyValueStore {
    pub fn open(path: &Path) -> SQLiteResult<Self> {
        Self::new(ManagedConnection::open(path)?)
    }

    pub fn open_in_memory() -> SQLiteResult<Self> {
        Self::new(ManagedConnection::open_in_memory()?)
    }

    pub fn new(conn: ManagedConnection) -> SQLiteResult<Self> {
        let store = Self {
            conn,
            table_ready: Arc::new(AtomicBool::new(false)),
        };
        store.ensure_table()?;
        Ok(store)
    }

    pub fn connection(&self) -> ManagedConnection {
        self.conn.clone()
    }

    /// Create an isolated transaction session over the same `SQLite` database.
    pub fn new_session(&self) -> Self {
        Self {
            conn: self.conn.new_session(),
            table_ready: Arc::clone(&self.table_ready),
        }
    }

    fn ensure_table(&self) -> SQLiteResult<()> {
        if self.table_ready.load(Ordering::Acquire) {
            return Ok(());
        }
        self.conn.with(|conn| {
            conn.execute(
                &format!(
                    "CREATE TABLE IF NOT EXISTS {KEY_VALUE_TABLE} (
                        key   BLOB PRIMARY KEY,
                        value BLOB NOT NULL
                    ) WITHOUT ROWID"
                ),
                [],
            )?;
            Ok(())
        })?;
        self.table_ready.store(true, Ordering::Release);
        Ok(())
    }
}

impl KeyValueStore for SQLiteKeyValueStore {
    fn storage_identity(&self) -> StorageBackendResult<Option<PersistentStorageIdentity>> {
        let Some(path) = self.conn.database_path() else {
            return Ok(None);
        };
        PersistentStorageIdentity::for_database_path(path).map(Some)
    }

    fn open_session(&self) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
        Ok(Arc::new(self.new_session()))
    }

    fn get(&self, key: &[u8]) -> StorageBackendResult<Option<Vec<u8>>> {
        self.ensure_table()?;
        Ok(self.conn.with(|conn| {
            conn.query_row(
                &format!("SELECT value FROM {KEY_VALUE_TABLE} WHERE key = ?1"),
                params![key],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(SQLiteError::from)
        })?)
    }

    fn contains_key(&self, key: &[u8]) -> StorageBackendResult<bool> {
        self.ensure_table()?;
        Ok(self.conn.with(|conn| {
            conn.query_row(
                &format!("SELECT 1 FROM {KEY_VALUE_TABLE} WHERE key = ?1 LIMIT 1"),
                params![key],
                |_| Ok(()),
            )
            .optional()
            .map(|value| value.is_some())
            .map_err(SQLiteError::from)
        })?)
    }

    fn put(&self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.ensure_table()?;
        self.conn.with(|conn| {
            conn.execute(
                &format!("INSERT OR REPLACE INTO {KEY_VALUE_TABLE} (key, value) VALUES (?1, ?2)"),
                params![key, value],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> StorageBackendResult<()> {
        self.ensure_table()?;
        self.conn.with(|conn| {
            conn.execute(
                &format!("DELETE FROM {KEY_VALUE_TABLE} WHERE key = ?1"),
                params![key],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    fn scan_prefix(&self, prefix: &[u8]) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.ensure_table()?;
        let upper = prefix_upper_bound(prefix);
        self.conn
            .with(|conn| {
                let mut rows = Vec::new();
                if let Some(upper) = upper {
                    let mut stmt = conn.prepare(&format!(
                        "SELECT key, value FROM {KEY_VALUE_TABLE}
                         WHERE key >= ?1 AND key < ?2
                         ORDER BY key"
                    ))?;
                    let iter = stmt.query_map(params![prefix, upper], |row| {
                        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                    })?;
                    for row in iter {
                        rows.push(row?);
                    }
                } else {
                    let mut stmt = conn.prepare(&format!(
                        "SELECT key, value FROM {KEY_VALUE_TABLE}
                         WHERE key >= ?1
                         ORDER BY key"
                    ))?;
                    let iter = stmt.query_map(params![prefix], |row| {
                        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                    })?;
                    for row in iter {
                        rows.push(row?);
                    }
                }
                Ok(rows)
            })
            .map_err(StorageBackendError::from)
    }

    fn scan_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.ensure_table()?;
        let upper = prefix_upper_bound(prefix);
        let after = after.filter(|after| *after >= prefix);
        let limit = i64::try_from(limit).map_err(|_| {
            StorageBackendError::Other(format!(
                "key/value cursor limit {limit} is outside SQLite's integer range"
            ))
        })?;
        self.conn
            .with(|connection| {
                let mut output = Vec::new();
                match (after, upper) {
                    (Some(after), Some(upper)) => {
                        let mut statement = connection.prepare_cached(&format!(
                            "SELECT key, value FROM {KEY_VALUE_TABLE}
                             WHERE key > ?1 AND key < ?2
                             ORDER BY key LIMIT ?3"
                        ))?;
                        let rows = statement.query_map(params![after, upper, limit], |row| {
                            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                        })?;
                        for row in rows {
                            output.push(row?);
                        }
                    }
                    (Some(after), None) => {
                        let mut statement = connection.prepare_cached(&format!(
                            "SELECT key, value FROM {KEY_VALUE_TABLE}
                             WHERE key > ?1
                             ORDER BY key LIMIT ?2"
                        ))?;
                        let rows = statement.query_map(params![after, limit], |row| {
                            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                        })?;
                        for row in rows {
                            output.push(row?);
                        }
                    }
                    (None, Some(upper)) => {
                        let mut statement = connection.prepare_cached(&format!(
                            "SELECT key, value FROM {KEY_VALUE_TABLE}
                             WHERE key >= ?1 AND key < ?2
                             ORDER BY key LIMIT ?3"
                        ))?;
                        let rows = statement.query_map(params![prefix, upper, limit], |row| {
                            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                        })?;
                        for row in rows {
                            output.push(row?);
                        }
                    }
                    (None, None) => {
                        let mut statement = connection.prepare_cached(&format!(
                            "SELECT key, value FROM {KEY_VALUE_TABLE}
                             WHERE key >= ?1
                             ORDER BY key LIMIT ?2"
                        ))?;
                        let rows = statement.query_map(params![prefix, limit], |row| {
                            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                        })?;
                        for row in rows {
                            output.push(row?);
                        }
                    }
                }
                Ok(output)
            })
            .map_err(StorageBackendError::from)
    }

    fn scan_prefix_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<Vec<Vec<u8>>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.ensure_table()?;
        let upper = prefix_upper_bound(prefix);
        let after = after.filter(|after| *after >= prefix);
        let limit = i64::try_from(limit).map_err(|_| {
            StorageBackendError::Other(format!(
                "key/value cursor limit {limit} is outside SQLite's integer range"
            ))
        })?;
        self.conn
            .with(|connection| {
                let mut keys = Vec::new();
                match (after, upper) {
                    (Some(after), Some(upper)) => {
                        let mut stmt = connection.prepare_cached(&format!(
                            "SELECT key FROM {KEY_VALUE_TABLE}
                             WHERE key > ?1 AND key < ?2
                             ORDER BY key LIMIT ?3"
                        ))?;
                        let rows =
                            stmt.query_map(params![after, upper, limit], |row| row.get(0))?;
                        for key in rows {
                            keys.push(key?);
                        }
                    }
                    (Some(after), None) => {
                        let mut stmt = connection.prepare_cached(&format!(
                            "SELECT key FROM {KEY_VALUE_TABLE}
                             WHERE key > ?1
                             ORDER BY key LIMIT ?2"
                        ))?;
                        let rows = stmt.query_map(params![after, limit], |row| row.get(0))?;
                        for key in rows {
                            keys.push(key?);
                        }
                    }
                    (None, Some(upper)) => {
                        let mut stmt = connection.prepare_cached(&format!(
                            "SELECT key FROM {KEY_VALUE_TABLE}
                             WHERE key >= ?1 AND key < ?2
                             ORDER BY key LIMIT ?3"
                        ))?;
                        let rows =
                            stmt.query_map(params![prefix, upper, limit], |row| row.get(0))?;
                        for key in rows {
                            keys.push(key?);
                        }
                    }
                    (None, None) => {
                        let mut stmt = connection.prepare_cached(&format!(
                            "SELECT key FROM {KEY_VALUE_TABLE}
                             WHERE key >= ?1
                             ORDER BY key LIMIT ?2"
                        ))?;
                        let rows = stmt.query_map(params![prefix, limit], |row| row.get(0))?;
                        for key in rows {
                            keys.push(key?);
                        }
                    }
                }
                Ok(keys)
            })
            .map_err(StorageBackendError::from)
    }

    fn first_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
    ) -> StorageBackendResult<Option<(Vec<u8>, Vec<u8>)>> {
        self.ensure_table()?;
        let upper = prefix_upper_bound(prefix);
        let after = after.filter(|after| *after >= prefix);
        self.conn
            .with(|connection| {
                let row = match (after, upper) {
                    (Some(after), Some(upper)) => connection
                        .prepare_cached(&format!(
                            "SELECT key, value FROM {KEY_VALUE_TABLE}
                             WHERE key > ?1 AND key < ?2
                             ORDER BY key LIMIT 1"
                        ))?
                        .query_row(params![after, upper], |row| {
                            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                        })
                        .optional()?,
                    (Some(after), None) => connection
                        .prepare_cached(&format!(
                            "SELECT key, value FROM {KEY_VALUE_TABLE}
                             WHERE key > ?1
                             ORDER BY key LIMIT 1"
                        ))?
                        .query_row(params![after], |row| {
                            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                        })
                        .optional()?,
                    (None, Some(upper)) => connection
                        .prepare_cached(&format!(
                            "SELECT key, value FROM {KEY_VALUE_TABLE}
                             WHERE key >= ?1 AND key < ?2
                             ORDER BY key LIMIT 1"
                        ))?
                        .query_row(params![prefix, upper], |row| {
                            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                        })
                        .optional()?,
                    (None, None) => connection
                        .prepare_cached(&format!(
                            "SELECT key, value FROM {KEY_VALUE_TABLE}
                             WHERE key >= ?1
                             ORDER BY key LIMIT 1"
                        ))?
                        .query_row(params![prefix], |row| {
                            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                        })
                        .optional()?,
                };
                Ok(row.filter(|(key, _)| key.starts_with(prefix)))
            })
            .map_err(StorageBackendError::from)
    }

    fn delete_prefix(&self, prefix: &[u8]) -> StorageBackendResult<usize> {
        self.ensure_table()?;
        let upper = prefix_upper_bound(prefix);
        let deleted = self.conn.with(|conn| {
            let deleted = if let Some(upper) = upper {
                conn.execute(
                    &format!(
                        "DELETE FROM {KEY_VALUE_TABLE}
                         WHERE key >= ?1 AND key < ?2"
                    ),
                    params![prefix, upper],
                )?
            } else {
                conn.execute(
                    &format!("DELETE FROM {KEY_VALUE_TABLE} WHERE key >= ?1"),
                    params![prefix],
                )?
            };
            Ok(deleted)
        })?;
        Ok(deleted)
    }

    fn batch(&self) -> Box<dyn KeyValueBatch + '_> {
        Box::new(SQLiteKeyValueBatch {
            store: self,
            operations: Vec::new(),
        })
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

struct SQLiteKeyValueBatch<'a> {
    store: &'a SQLiteKeyValueStore,
    operations: Vec<SQLiteKeyValueBatchOperation>,
}

impl KeyValueBatch for SQLiteKeyValueBatch<'_> {
    fn put(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.operations.push(SQLiteKeyValueBatchOperation::Put(
            key.to_vec(),
            value.to_vec(),
        ));
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(SQLiteKeyValueBatchOperation::Delete(key.to_vec()));
        Ok(())
    }

    fn delete_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.operations
            .push(SQLiteKeyValueBatchOperation::DeletePrefix(prefix.to_vec()));
        Ok(())
    }

    fn commit(self: Box<Self>) -> StorageBackendResult<()> {
        self.store.ensure_table()?;
        self.store.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            for operation in self.operations {
                match operation {
                    SQLiteKeyValueBatchOperation::Put(key, value) => {
                        tx.execute(
                            &format!(
                                "INSERT OR REPLACE INTO {KEY_VALUE_TABLE} (key, value)
                                 VALUES (?1, ?2)"
                            ),
                            params![key, value],
                        )?;
                    }
                    SQLiteKeyValueBatchOperation::Delete(key) => {
                        tx.execute(
                            &format!("DELETE FROM {KEY_VALUE_TABLE} WHERE key = ?1"),
                            params![key],
                        )?;
                    }
                    SQLiteKeyValueBatchOperation::DeletePrefix(prefix) => {
                        if let Some(upper) = prefix_upper_bound(&prefix) {
                            tx.execute(
                                &format!(
                                    "DELETE FROM {KEY_VALUE_TABLE}
                                     WHERE key >= ?1 AND key < ?2"
                                ),
                                params![prefix, upper],
                            )?;
                        } else {
                            tx.execute(
                                &format!("DELETE FROM {KEY_VALUE_TABLE} WHERE key >= ?1"),
                                params![prefix],
                            )?;
                        }
                    }
                }
            }
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }
}

/// Shared `SQLite` `KeyValue` storage handle with catalog and backend factories.
#[derive(Clone)]
pub struct SQLiteKeyValueStorage {
    store: Arc<SQLiteKeyValueStore>,
}

pub type SQLiteKeyValueCatalog = KeyValueCatalog;
pub type SQLiteKeyValueStorageBackend = KeyValueStorageBackend;

impl SQLiteKeyValueStorage {
    pub fn open(path: &Path) -> SQLiteResult<Self> {
        Ok(Self {
            store: Arc::new(SQLiteKeyValueStore::open(path)?),
        })
    }

    pub fn open_in_memory() -> SQLiteResult<Self> {
        Ok(Self {
            store: Arc::new(SQLiteKeyValueStore::open_in_memory()?),
        })
    }

    pub fn from_connection(conn: ManagedConnection) -> SQLiteResult<Self> {
        Ok(Self {
            store: Arc::new(SQLiteKeyValueStore::new(conn)?),
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
