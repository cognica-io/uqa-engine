//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use uqa_storage::{
    DocumentStore, InvertedIndex, PersistentStorageBackend, StorageBackendResult,
    StorageSavepointId, VectorIndex, VectorIndexOpenMode, VectorIndexSpec,
};

use super::{Catalog, Engine, ManagedConnection};

/// Commit from another connection precisely when catalog restoration opens
/// its first table, after the reader captured its storage generation.
struct CommitDuringRestore {
    inner: Arc<dyn PersistentStorageBackend>,
    reader: ManagedConnection,
    writer: ManagedConnection,
    armed: AtomicBool,
}

impl PersistentStorageBackend for CommitDuringRestore {
    fn document_store(&self, table: &str) -> Box<dyn DocumentStore> {
        if self.armed.swap(false, Ordering::AcqRel) {
            assert!(
                self.inner.in_transaction(),
                "external catalog restoration must own a pinned read transaction"
            );
            Catalog::open(self.writer.clone())
                .unwrap()
                .set_metadata("refresh_probe", "after")
                .unwrap();
            assert_eq!(
                Catalog::open(self.reader.clone())
                    .unwrap()
                    .get_metadata("refresh_probe")
                    .unwrap()
                    .as_deref(),
                Some("before"),
                "one catalog restoration must not mix committed generations"
            );
        }
        self.inner.document_store(table)
    }

    fn inverted_index(
        &self,
        table: &str,
        analyzer: uqa_analysis::Analyzer,
    ) -> Box<dyn InvertedIndex> {
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
        self.inner
            .vector_index(table, field, dimensions, spec, mode)
    }

    fn begin_transaction(&self) -> StorageBackendResult<()> {
        self.inner.begin_transaction()
    }

    fn begin_read_transaction(&self) -> StorageBackendResult<()> {
        self.inner.begin_read_transaction()
    }

    fn in_transaction(&self) -> bool {
        self.inner.in_transaction()
    }

    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        self.inner.transaction_has_written()
    }

    fn change_version(&self) -> StorageBackendResult<Option<u64>> {
        self.inner.change_version()
    }

    fn change_version_monitor_is_nonblocking(&self) -> StorageBackendResult<bool> {
        self.inner.change_version_monitor_is_nonblocking()
    }

    fn pin_transaction_snapshot(&self) -> StorageBackendResult<()> {
        self.inner.pin_transaction_snapshot()
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

#[test]
fn external_refresh_keeps_one_snapshot_when_a_writer_commits_during_restore() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("external-refresh-race.db");
    let reader = ManagedConnection::open(&path).unwrap();
    let catalog = Catalog::open(reader.clone()).unwrap();
    let mut engine = Engine::from_persistent_backends(
        Arc::new(catalog),
        Arc::new(uqa_storage_sqlite::SQLiteStorageBackend::new(
            reader.clone(),
        )),
    )
    .unwrap();
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    engine
        .sql(
            "CREATE TABLE items (id INTEGER); CREATE TABLE audit (id INTEGER); \
             CREATE RULE item_audit AS ON INSERT TO items \
             DO ALSO INSERT INTO audit VALUES (NEW.id)",
            &[],
        )
        .unwrap();
    engine.synchronize_catalog_registries().unwrap();
    let writer = ManagedConnection::open(&path).unwrap();
    Catalog::open(writer.clone())
        .unwrap()
        .set_metadata("refresh_probe", "before")
        .unwrap();
    let backend = Arc::new(CommitDuringRestore {
        inner: engine.storage.backend.take().unwrap(),
        reader,
        writer,
        armed: AtomicBool::new(true),
    });
    engine.storage.backend = Some(backend.clone());

    engine.synchronize_catalog_registries().unwrap();

    assert!(!backend.armed.load(Ordering::Acquire));
    assert!(
        !backend.in_transaction(),
        "refresh must release its snapshot"
    );
    let observed = engine
        .epochs
        .seen_storage_change_version
        .load(Ordering::Acquire);
    assert_ne!(Some(observed), backend.change_version().unwrap());
    engine.synchronize_catalog_registries().unwrap();
    assert_eq!(
        Some(
            engine
                .epochs
                .seen_storage_change_version
                .load(Ordering::Acquire)
        ),
        backend.change_version().unwrap(),
        "a commit racing restoration must still be observed on the next refresh"
    );
    engine.sql("INSERT INTO items VALUES (7)", &[]).unwrap();
    assert_eq!(
        engine.sql("SELECT id FROM audit", &[]).unwrap().rows.len(),
        1
    );
}
