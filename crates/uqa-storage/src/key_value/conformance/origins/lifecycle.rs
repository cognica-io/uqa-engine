//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Origin allocation resolves through real provider receipts, including failed evaluation and lost replies.

use super::super::{expect, expect_eq};
use crate::mvcc::{
    CommitResult, CommitStatus, CommittedRecordSnapshot, DatabaseId, IdentifierAllocation,
    IdentifierRequest, PreparedRecordCommit, ReceiptAcknowledgement, RetainedTransactionAllocation,
    SerializableCoordinator, StorageMutationOrigin, StorageTransactionId, VersionError,
    VersionResult, VersionedKeyValueStore, VersionedPersistence, VersionedSessionOptions,
};
use crate::read_control::StorageReadControl;
use crate::{KeyValueStore, StorageBackendError, StorageBackendResult};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Verify real writer allocation, monotonic revisions, undo, failure cleanup and retry without evaluation replay on a fresh disposable physical store.
pub fn verify_mutation_origins(
    persistence: Arc<dyn VersionedPersistence>,
) -> StorageBackendResult<()> {
    let faults = Arc::new(Faults {
        inner: persistence,
        lose_commit: AtomicBool::new(false),
        fail_abort: AtomicBool::new(false),
        fail_acknowledgement: AtomicBool::new(false),
    });
    let store =
        VersionedKeyValueStore::new(faults.clone(), None, VersionedSessionOptions::default());
    let control = StorageReadControl::with_limit(1 << 20);
    store.begin_transaction()?;
    let first = mutate(&store, b"first")?;
    expect(
        store.pending_commit().is_none() && store.pending_transaction_completion().is_none(),
        "active origin is not a sealed commit attempt",
    )?;
    store.savepoint("origin")?;
    let undone = mutate(&store, b"undone")?;
    store.rollback_to_savepoint("origin")?;
    let mut failed = None;
    expect(
        store
            .with_versioned_mutation(&mut |origin, _, batch| {
                failed = Some(origin);
                batch.put(b"origin-value", b"discarded")?;
                Err(rejected())
            })
            .is_err(),
        "failed private origin evaluation propagates",
    )?;
    let next = mutate(&store, b"last")?;
    expect(
        first.transaction() == next.transaction()
            && next.revision() > failed.expect("invoked").revision()
            && undone.revision() > first.revision(),
        "revisions survive statement and savepoint undo",
    )?;
    store.release_savepoint("origin")?;
    store.commit_transaction()?;
    expect(
        matches!(faults.inner.commit_status(first.transaction(), &control).map_err(VersionError::into_storage_error)?, CommitStatus::Committed(receipt) if receipt.transaction == first.transaction()),
        "origin writer is the actual committed receipt",
    )?;
    expect_eq(
        &store.get(b"origin-value")?,
        &Some(b"last".to_vec()),
        "only surviving evaluation is published",
    )?;
    empty_and_serializable(&store, &faults, &control)?;
    failed_autocommit(&store, &faults, &control)?;
    lost_commit(&store, &faults)?;
    lost_canonical_commit(&faults, &control)?;
    store.begin_read_transaction()?;
    let mut invoked = false;
    expect(
        store
            .with_versioned_mutation(&mut |_, _, _| {
                invoked = true;
                Ok(())
            })
            .is_err()
            && !invoked,
        "read-only mutation rejects before evaluation",
    )?;
    store.rollback_transaction()?;
    Ok(())
}

fn mutate(store: &dyn KeyValueStore, value: &[u8]) -> StorageBackendResult<StorageMutationOrigin> {
    let mut selected = None;
    store.with_versioned_mutation(&mut |origin, _, batch| {
        selected = Some(origin);
        batch.put(b"origin-value", value)
    })?;
    selected.ok_or_else(rejected)
}

fn empty_and_serializable(
    store: &VersionedKeyValueStore,
    faults: &Faults,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    store.begin_transaction()?;
    store.savepoint("empty")?;
    let empty = mutate(store, b"empty")?;
    store.rollback_to_savepoint("empty")?;
    store.release_savepoint("empty")?;
    store.commit_transaction()?;
    expect(
        !matches!(
            faults
                .inner
                .commit_status(empty.transaction(), control)
                .map_err(VersionError::into_storage_error)?,
            CommitStatus::Pending
        ),
        "all-undone origin allocation completes",
    )?;
    expect_eq(
        &store.get(b"origin-value")?,
        &Some(b"last".to_vec()),
        "empty commit preserves prior values",
    )?;
    store.begin_transaction()?;
    store.establish_serializable_snapshot()?;
    let origin = mutate(store, b"ssi")?;
    store.rollback_transaction()?;
    expect_eq(
        &faults
            .inner
            .commit_status(origin.transaction(), control)
            .map_err(VersionError::into_storage_error)?,
        &CommitStatus::Aborted,
        "unprepared SSI origin allocation aborts",
    )?;
    Ok(())
}

fn failed_autocommit(
    store: &VersionedKeyValueStore,
    faults: &Faults,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    for mode in 0..5 {
        let mut origin = None;
        let mut calls = 0;
        faults.fail_abort.store(mode == 3, Ordering::SeqCst);
        faults
            .fail_acknowledgement
            .store(mode == 4, Ordering::SeqCst);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.with_versioned_mutation(&mut |current, read, batch| {
                calls += 1;
                origin = Some(current);
                batch.put(b"origin-value", b"failed autocommit")?;
                assert!(mode != 1, "injected origin evaluation unwind");
                if mode == 2 {
                    read.control().cancellation().cancel();
                    return Ok(());
                }
                Err(rejected())
            })
        }));
        store.retention_control().cancellation().reset();
        expect(
            if mode == 1 {
                result.is_err()
            } else {
                matches!(result, Ok(Err(_)))
            },
            "original evaluation failure propagates",
        )?;
        expect_eq(&calls, &1, "failed evaluation is not replayed")?;
        let transaction = origin.expect("invoked").transaction();
        if mode >= 3 {
            expect_eq(
                &store.pending_commit(),
                &Some(transaction),
                "failed abort retains exact origin attempt",
            )?;
            expect(
                store.commit_transaction().is_err() && mutate(store, b"forbidden").is_err(),
                "failed evaluation cannot publish or accept further writes",
            )?;
            store.rollback_transaction()?;
        }
        expect(
            !store.in_transaction(),
            "failed autocommit owner released after abort",
        )?;
        expect_eq(
            &faults
                .inner
                .commit_status(transaction, control)
                .map_err(VersionError::into_storage_error)?,
            &CommitStatus::Aborted,
            "failed origin receipt aborted",
        )?;
        faults
            .inner
            .reclaim_transaction_receipts(control)
            .map_err(VersionError::into_storage_error)?;
        expect_eq(
            &faults
                .inner
                .commit_status(transaction, control)
                .map_err(VersionError::into_storage_error)?,
            &CommitStatus::Unknown,
            "failed origin receipt acknowledged for reclamation",
        )?;
        expect_eq(
            &store.get(b"origin-value")?,
            &Some(b"last".to_vec()),
            "failed origin publishes no partial records",
        )?;
    }
    Ok(())
}

fn lost_commit(store: &VersionedKeyValueStore, faults: &Faults) -> StorageBackendResult<()> {
    faults.lose_commit.store(true, Ordering::SeqCst);
    let mut selected = None;
    let mut calls = 0;
    let result = store.with_versioned_mutation(&mut |origin, _, batch| {
        calls += 1;
        selected = Some(origin);
        batch.put(
            b"origin-value",
            &origin.transaction().allocation().to_le_bytes(),
        )
    });
    let origin = selected.expect("invoked");
    expect(result.is_err(), "lost publication reply propagates")?;
    expect_eq(
        &store.pending_commit(),
        &Some(origin.transaction()),
        "publication retry keeps original writer",
    )?;
    store.commit_transaction()?;
    expect_eq(
        &calls,
        &1,
        "publication retry never replays origin evaluation",
    )?;
    expect_eq(
        &store.get(b"origin-value")?,
        &Some(origin.transaction().allocation().to_le_bytes().to_vec()),
        "retry publishes the original evaluated origin bytes",
    )
}

fn lost_canonical_commit(
    faults: &Arc<Faults>,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    use crate::diskann_index::format::DiskANNChangeIdentity;
    use crate::key_value::{vector_index::origin::journal, KeyValueDiskANNCanonical};
    let session = Arc::new(VersionedKeyValueStore::new(
        faults.clone(),
        None,
        VersionedSessionOptions::default(),
    ));
    let store: Arc<dyn KeyValueStore> = session.clone();
    let canonical = KeyValueDiskANNCanonical::new(store.clone(), "lost-change", "embedding", 2)?;
    faults.lose_commit.store(true, Ordering::SeqCst);
    expect(
        canonical.replace(9, &[vec![3.0, 4.0]], control).is_err(),
        "lost canonical commit reply propagates",
    )?;
    let pending = session.pending_commit().expect("retained commit attempt");
    store.commit_transaction()?;
    let source = canonical.retain(control)?;
    let origin = source
        .origin(9, control)?
        .expect("committed canonical origin");
    expect_eq(
        &origin.writer(),
        &pending,
        "retry preserves the actual canonical writer",
    )?;
    expect_eq(
        &origin.revision(),
        &1,
        "retry does not reevaluate canonical mutation",
    )?;
    expect_eq(
        &source.next_change_after(None, control)?,
        &Some(DiskANNChangeIdentity::new(9, origin)),
        "change and canonical data commit together after lost reply",
    )?;
    let mut count = 0;
    let prefix = journal::prefix("lost-change", "embedding")?;
    store.with_read_view(&mut |read| {
        read.visit_keys_after(&prefix, None, usize::MAX, control, &mut |_| {
            count += 1;
            Ok(())
        })
    })?;
    expect_eq(
        &count,
        &1,
        "commit retry does not duplicate a change record",
    )?;
    super::values(&source, 9, &[vec![3.0, 4.0]], control)
}

fn rejected() -> StorageBackendError {
    StorageBackendError::Other("origin evaluation rejected".into())
}

struct Faults {
    inner: Arc<dyn VersionedPersistence>,
    lose_commit: AtomicBool,
    fail_abort: AtomicBool,
    fail_acknowledgement: AtomicBool,
}

impl VersionedPersistence for Faults {
    fn database_id(&self) -> DatabaseId {
        self.inner.database_id()
    }
    fn serializable_coordinator(&self) -> Option<&dyn SerializableCoordinator> {
        self.inner.serializable_coordinator()
    }
    fn identifier_watermark(
        &self,
        namespace: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<u64>> {
        self.inner.identifier_watermark(namespace, control)
    }
    fn allocate_identifiers(
        &self,
        namespace: &[u8],
        request: IdentifierRequest,
        control: &StorageReadControl,
    ) -> VersionResult<IdentifierAllocation> {
        self.inner.allocate_identifiers(namespace, request, control)
    }
    fn allocate_transaction(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<StorageTransactionId> {
        self.inner.allocate_transaction(control)
    }
    fn allocate_managed_transaction(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<RetainedTransactionAllocation> {
        self.inner.allocate_managed_transaction(control)
    }
    fn snapshot(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Arc<dyn CommittedRecordSnapshot>> {
        self.inner.snapshot(control)
    }
    fn reclaim_versions(&self, control: &StorageReadControl) -> VersionResult<u64> {
        self.inner.reclaim_versions(control)
    }
    fn commit(
        &self,
        transaction: StorageTransactionId,
        prepared: &PreparedRecordCommit,
        control: &StorageReadControl,
    ) -> CommitResult {
        let receipt = self.inner.commit(transaction, prepared, control)?;
        if self.lose_commit.swap(false, Ordering::SeqCst) {
            return Err(crate::mvcc::CommitFailure::Indeterminate {
                transaction,
                source: rejected(),
            });
        }
        Ok(receipt)
    }
    fn commit_status(
        &self,
        transaction: StorageTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<CommitStatus> {
        self.inner.commit_status(transaction, control)
    }
    fn acknowledge_transaction(
        &self,
        acknowledgement: ReceiptAcknowledgement,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        if self.fail_acknowledgement.swap(false, Ordering::SeqCst) {
            return Err(VersionError::InvalidEncoding(
                "injected origin acknowledgement failure",
            ));
        }
        self.inner.acknowledge_transaction(acknowledgement, control)
    }
    fn reclaim_transaction_receipts(&self, control: &StorageReadControl) -> VersionResult<u64> {
        self.inner.reclaim_transaction_receipts(control)
    }
    fn abort(
        &self,
        transaction: StorageTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<CommitStatus> {
        if self.fail_abort.swap(false, Ordering::SeqCst) {
            return Err(VersionError::InvalidEncoding(
                "injected origin abort failure",
            ));
        }
        self.inner.abort(transaction, control)
    }
}
