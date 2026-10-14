//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent sessions open each physical store once and retain current row counts.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use uqa_storage::{
    DocumentStore, InvertedIndex, PersistentStorageBackend, PersistentStorageProvider,
    PersistentStorageSession, StorageBackendResult, StorageSavepointId, VectorIndex,
    VectorIndexOpenMode, VectorIndexSpec,
};

use super::Engine;

fn pause_statistics(engine: &Engine) {
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
}

struct CountedStores {
    inner: Arc<dyn PersistentStorageBackend>,
    documents: AtomicUsize,
    inverted: AtomicUsize,
    vectors: AtomicUsize,
}

impl PersistentStorageBackend for CountedStores {
    fn transaction_affinity(&self) -> Option<uqa_storage::StorageSessionAffinity> {
        self.inner.transaction_affinity()
    }

    fn document_store(&self, table: &str) -> Box<dyn DocumentStore> {
        self.documents.fetch_add(1, Ordering::Relaxed);
        self.inner.document_store(table)
    }

    fn inverted_index(
        &self,
        table: &str,
        analyzer: uqa_analysis::Analyzer,
    ) -> Box<dyn InvertedIndex> {
        self.inverted.fetch_add(1, Ordering::Relaxed);
        self.inner.inverted_index(table, analyzer)
    }

    fn vector_index(
        &self,
        table: &str,
        field: &str,
        dimensions: u32,
        spec: VectorIndexSpec,
        mode: VectorIndexOpenMode,
    ) -> StorageBackendResult<Box<dyn VectorIndex>> {
        self.vectors.fetch_add(1, Ordering::Relaxed);
        self.inner
            .vector_index(table, field, dimensions, spec, mode)
    }

    fn change_version(&self) -> StorageBackendResult<Option<u64>> {
        self.inner.change_version()
    }

    fn read_view_revision(
        &self,
    ) -> StorageBackendResult<Option<uqa_storage::key_value::KeyValueReadRevision>> {
        self.inner.read_view_revision()
    }

    fn begin_transaction(&self) -> StorageBackendResult<()> {
        self.inner.begin_transaction()
    }

    fn in_transaction(&self) -> bool {
        self.inner.in_transaction()
    }

    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        self.inner.transaction_has_written()
    }

    fn commit_transaction(&self) -> StorageBackendResult<()> {
        self.inner.commit_transaction()
    }

    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        self.inner.rollback_transaction()
    }

    fn savepoint(&self, id: StorageSavepointId) -> StorageBackendResult<()> {
        self.inner.savepoint(id)
    }

    fn release_savepoint(&self, id: StorageSavepointId) -> StorageBackendResult<()> {
        self.inner.release_savepoint(id)
    }

    fn rollback_to_savepoint(&self, id: StorageSavepointId) -> StorageBackendResult<()> {
        self.inner.rollback_to_savepoint(id)
    }
}

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_kv(1)]
#[case::redb(2)]
fn shared_sessions_open_each_store_once_and_keep_current_counts(#[case] kind: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session-stores.db");
    let provider: Arc<dyn PersistentStorageProvider> = match kind {
        0 => Arc::new(uqa_storage_sqlite::SQLiteStorageProvider::new(
            uqa_storage_sqlite::ManagedConnection::open(&path).unwrap(),
        )),
        1 => Arc::new(uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap()),
        2 => Arc::new(uqa_storage_redb::RedbStorage::open(&path).unwrap()),
        _ => unreachable!(),
    };
    let root = Engine::from_persistent_provider(Arc::clone(&provider)).unwrap();
    pause_statistics(&root);
    root.sql("CREATE TABLE items (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(3)); CREATE INDEX items_body ON items USING gin(body) WITH(analyzer='keyword'); CREATE INDEX items_vector ON items USING hnsw(embedding); INSERT INTO items VALUES (1, 'ready now', ARRAY[1.0, 0.0, 0.0]); CREATE TABLE unrelated (id INTEGER)", &[]).unwrap();
    root.synchronize_table_catalog().unwrap();
    root.synchronize_table_data().unwrap();
    root.synchronize_catalog_registries().unwrap();
    for table in root.storage.tables.read().values() {
        table.stored_document_count().unwrap();
        assert!(!table.doc_count_dirty.load(Ordering::Acquire));
    }
    let storage = provider.open_session().unwrap();
    let counted = Arc::new(CountedStores {
        inner: storage.backend,
        documents: AtomicUsize::new(0),
        inverted: AtomicUsize::new(0),
        vectors: AtomicUsize::new(0),
    });
    let storage = PersistentStorageSession::new(storage.catalog, counted.clone());
    let seeded = root
        .session_from_shared_catalog(&storage, &provider)
        .unwrap()
        .expect("unchanged committed catalog must be shared");
    assert_eq!(counted.documents.load(Ordering::Relaxed), 2);
    assert_eq!(counted.inverted.load(Ordering::Relaxed), 2);
    assert_eq!(counted.vectors.load(Ordering::Relaxed), 1);
    for (name, original) in root.storage.tables.read().iter() {
        let tables = seeded.storage.tables.read();
        let copied = &tables[name];
        assert!(!copied.doc_count_dirty.load(Ordering::Acquire));
        assert_eq!(
            copied.doc_count_cache.load(Ordering::Acquire),
            original.doc_count_cache.load(Ordering::Acquire)
        );
        assert!(!Arc::ptr_eq(original, copied));
        assert!(Arc::ptr_eq(
            &original.columns.snapshot(),
            &copied.columns.snapshot()
        ));
    }
    let item = seeded.storage.tables.read()
        [&uqa_core::RelationIdentity::from_legacy_name("public.items").unwrap()]
        .clone();
    assert_eq!(item.vector_indexes.read()["embedding"].index_kind(), "hnsw");

    // Public sessions still own their transactions and observe later commits.
    let session = root.new_session().unwrap();
    pause_statistics(&session);
    let count = |engine: &Engine| {
        engine
            .sql("SELECT count(*) FROM items", &[])
            .unwrap()
            .value_at(0, 0)
            .cloned()
            .unwrap()
    };
    assert_eq!(count(&session), uqa_core::Value::Int(1));
    assert!(session
        .sql("SELECT id FROM items WHERE text_match(body, 'ready')", &[])
        .unwrap()
        .rows
        .is_empty());
    let hits = session.sql("SELECT id FROM items WHERE text_match(body, 'ready now') AND knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 1)", &[]).unwrap();
    assert_eq!(hits.rows.len(), 1);
    assert_eq!(hits.value_at(0, 0), Some(&uqa_core::Value::Int(1)));
    root.sql(
        "INSERT INTO items VALUES (2, 'later', ARRAY[0.0, 1.0, 0.0])",
        &[],
    )
    .unwrap();
    assert_eq!(count(&session), uqa_core::Value::Int(2));
    session
        .sql("BEGIN; DELETE FROM items WHERE id = 1; ROLLBACK", &[])
        .unwrap();
    assert_eq!(count(&session), uqa_core::Value::Int(2));
}
