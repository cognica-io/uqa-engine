//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! In-memory physical records exercise the graph owner's real versioned sessions without provider dependencies.

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::Mutex;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_storage::{mvcc::*, read_control::StorageReadControl};
use uqa_storage::{KeyValueCatalog, KeyValueStorageBackend, PersistentStorageSession};

#[derive(Default)]
struct Publication {
    next: u64,
    identifiers: BTreeMap<Vec<u8>, u64>,
    receipts: BTreeMap<u64, CommitStatus>,
}

pub(super) struct Database {
    records: MemoryVersionStore,
    publication: Mutex<Publication>,
    admission: Arc<LocalSerializableState>,
    owner: Arc<()>,
    graph: Mutex<SerializableGraph>,
}

impl Database {
    pub(super) fn new() -> Arc<Self> {
        let memory = MemoryBudget::new(1 << 24);
        Arc::new(Self {
            records: MemoryVersionStore::new(&memory),
            publication: Mutex::default(),
            admission: Arc::default(),
            owner: Arc::new(()),
            graph: Mutex::new(
                SerializableGraph::new(DatabaseId::from_bytes([9; 16]), [8; 16], &memory).unwrap(),
            ),
        })
    }

    pub(super) fn session(self: &Arc<Self>) -> PersistentStorageSession {
        let store = Arc::new(VersionedKeyValueStore::new_with_cancellation(
            self.clone(),
            None,
            VersionedSessionOptions {
                retained_bytes: 1 << 20,
            },
            CancellationToken::new(),
        ));
        PersistentStorageSession::new(
            Arc::new(KeyValueCatalog::new(store.clone())),
            Arc::new(KeyValueStorageBackend::new(store)),
        )
    }
}

impl SerializableCoordinator for Database {
    fn with_serializable_admission(
        &self,
        control: &StorageReadControl,
        operation: &mut SerializableOperation<'_>,
    ) -> VersionResult<()> {
        self.admission
            .with_admission(&self.owner, control, |leases| {
                operation(&mut self.graph.lock(), leases)
            })
    }
}

impl VersionedPersistence for Database {
    fn database_id(&self) -> DatabaseId {
        DatabaseId::from_bytes([9; 16])
    }

    fn serializable_coordinator(&self) -> Option<&dyn SerializableCoordinator> {
        Some(self)
    }

    fn graph_record_layout(&self) -> Option<&dyn GraphRecordLayout> {
        Some(&uqa_storage::key_value::KeyValueGraphRecords)
    }

    fn identifier_watermark(
        &self,
        namespace: &[u8],
        _: &StorageReadControl,
    ) -> VersionResult<Option<u64>> {
        Ok(self.publication.lock().identifiers.get(namespace).copied())
    }

    fn allocate_identifiers(
        &self,
        namespace: &[u8],
        request: IdentifierRequest,
        control: &StorageReadControl,
    ) -> VersionResult<IdentifierAllocation> {
        let _workspace = request.reserve_workspace(namespace, control)?;
        let mut publication = self.publication.lock();
        let allocation = request.prepare(publication.identifiers.get(namespace).copied())?;
        publication
            .identifiers
            .insert(namespace.to_vec(), allocation.watermark());
        Ok(allocation)
    }

    fn allocate_transaction(&self, _: &StorageReadControl) -> VersionResult<StorageTransactionId> {
        let mut publication = self.publication.lock();
        publication.next += 1;
        let id = StorageTransactionId::new(self.database_id(), publication.next)?;
        publication
            .receipts
            .insert(id.allocation(), CommitStatus::Pending);
        Ok(id)
    }

    fn snapshot(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Arc<dyn CommittedRecordSnapshot>> {
        retain_record_snapshot(self.records.snapshot()?, control)
    }

    fn reclaim_versions(&self, _: &StorageReadControl) -> VersionResult<u64> {
        self.records.reclaim().map(|count| count as u64)
    }

    fn commit(
        &self,
        transaction: StorageTransactionId,
        prepared: &PreparedRecordCommit,
        control: &StorageReadControl,
    ) -> CommitResult {
        let mut publication = self.publication.lock();
        let status = publication.receipts[&transaction.allocation()];
        if let Some(receipt) =
            resolve_prepared_receipt(status, transaction, prepared.fingerprint())?
        {
            return Ok(receipt);
        }
        let receipt = CommitReceipt {
            transaction,
            sequence: self.records.commit_prepared(prepared, control)?,
            fingerprint: prepared.fingerprint(),
        };
        publication
            .receipts
            .insert(transaction.allocation(), CommitStatus::Committed(receipt));
        Ok(receipt)
    }

    fn commit_status(
        &self,
        transaction: StorageTransactionId,
        _: &StorageReadControl,
    ) -> VersionResult<CommitStatus> {
        Ok(self
            .publication
            .lock()
            .receipts
            .get(&transaction.allocation())
            .copied()
            .unwrap_or(CommitStatus::Unknown))
    }

    fn abort(
        &self,
        transaction: StorageTransactionId,
        _: &StorageReadControl,
    ) -> VersionResult<CommitStatus> {
        let mut publication = self.publication.lock();
        let status = publication
            .receipts
            .entry(transaction.allocation())
            .or_insert(CommitStatus::Unknown);
        if *status == CommitStatus::Pending {
            *status = CommitStatus::Aborted;
        }
        Ok(*status)
    }
}
