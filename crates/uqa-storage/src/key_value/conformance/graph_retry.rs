//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Force a real intervening commit after effect preparation and before physical admission.

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use parking_lot::Mutex;

use crate::mvcc::{
    CommitFingerprint, CommitResult, CommitStatus, CommittedRecordSnapshot, DatabaseId,
    GraphRecordLayout, PreparedRecordCommit, StorageTransactionId, VersionError, VersionResult,
    VersionedKeyValueStore, VersionedPersistence, VersionedSessionOptions,
};
use crate::{
    read_control::StorageReadControl, CatalogFacade, KeyValueCatalog, KeyValueStore,
    StorageBackendResult,
};

use super::expect;

struct InterleavingPersistence {
    inner: Arc<dyn VersionedPersistence>,
    armed: AtomicBool,
    observed: AtomicBool,
    attempts: AtomicUsize,
    allocations: AtomicUsize,
    identity: Mutex<Option<(StorageTransactionId, CommitFingerprint)>>,
}

impl VersionedPersistence for InterleavingPersistence {
    fn database_id(&self) -> DatabaseId {
        self.inner.database_id()
    }
    fn graph_record_layout(&self) -> Option<&dyn GraphRecordLayout> {
        self.inner.graph_record_layout()
    }
    fn allocate_transaction(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<StorageTransactionId> {
        if self.observed.load(Ordering::SeqCst) {
            self.allocations.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.allocate_transaction(control)
    }
    fn snapshot(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Arc<dyn CommittedRecordSnapshot>> {
        self.inner.snapshot(control)
    }
    fn commit(
        &self,
        transaction: StorageTransactionId,
        prepared: &PreparedRecordCommit,
        control: &StorageReadControl,
    ) -> CommitResult {
        if self.observed.load(Ordering::SeqCst) {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            let mut identity = self.identity.lock();
            let current = (transaction, prepared.fingerprint());
            if identity.is_some_and(|previous| previous != current) {
                return Err(VersionError::CommitMismatch.into());
            }
            *identity = Some(current);
        }
        if self.armed.swap(false, Ordering::SeqCst) {
            let other = VersionedKeyValueStore::new(
                self.inner.clone(),
                None,
                VersionedSessionOptions::default(),
            );
            other
                .put(b"graph-admission-independent", b"preserved")
                .map_err(VersionError::Storage)?;
        }
        self.inner.commit(transaction, prepared, control)
    }
    fn commit_status(
        &self,
        transaction: StorageTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<CommitStatus> {
        self.inner.commit_status(transaction, control)
    }
    fn abort(
        &self,
        transaction: StorageTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<CommitStatus> {
        self.inner.abort(transaction, control)
    }
}

/// Run against fresh disposable Key/Value record persistence. Verify that real physical admission rejects a stale dependency snapshot, retries only derived effects with the same transaction and fingerprint, and preserves the competing commit.
pub fn verify_graph_admission_retry(
    records: Arc<dyn VersionedPersistence>,
) -> StorageBackendResult<()> {
    let persistence = Arc::new(InterleavingPersistence {
        inner: records,
        armed: AtomicBool::new(false),
        observed: AtomicBool::new(false),
        attempts: AtomicUsize::new(0),
        allocations: AtomicUsize::new(0),
        identity: Mutex::new(None),
    });
    let store = Arc::new(VersionedKeyValueStore::new(
        persistence.clone(),
        None,
        VersionedSessionOptions::default(),
    ));
    let catalog = KeyValueCatalog::new(store.clone());
    catalog.save_named_graph("graph-admission")?;
    catalog.save_vertex(1, "node", "{}")?;
    catalog.save_graph_membership("vertex", 1, "graph-admission")?;
    catalog.save_path_index("graph-admission", "[]")?;
    catalog.finish_path_index_data("graph-admission", "graph-admission", "[]")?;
    store.begin_transaction()?;
    catalog.save_vertex(1, "node", "{\"changed\":true}")?;
    persistence.observed.store(true, Ordering::SeqCst);
    persistence.armed.store(true, Ordering::SeqCst);
    store.commit_transaction()?;
    expect(
        persistence.attempts.load(Ordering::SeqCst) == 2,
        "derived dependency snapshot must be retried",
    )?;
    expect(
        persistence.allocations.load(Ordering::SeqCst) == 1,
        "derived retry keeps its transaction allocation",
    )?;
    expect(
        store.get(b"graph-admission-independent")?.as_deref() == Some(b"preserved".as_slice()),
        "intervening physical commit survives",
    )?;
    expect(
        catalog
            .graph_vertex(1)?
            .is_some_and(|row| row.properties_json == "{\"changed\":true}"),
        "sealed graph source is published",
    )?;
    expect(
        !catalog.path_index_data_is_current("graph-admission", "[]")?,
        "derived cache is invalidated after retry",
    )
}
