//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical Key/Value sessions retain private changes rather than a physical writer.

mod batch;
mod read;
mod serializable;
pub use serializable::{SerializableReadContext, SerializableSession};
mod transaction;

use std::sync::Arc;

use parking_lot::Mutex;

use crate::read_control::{KeyValueReadVisitor, StorageReadControl, ValueReadVisitor};
use crate::{
    KeyValueBatch, KeyValueStore, PersistentStorageIdentity, StorageBackendError,
    StorageBackendResult, StorageSessionAffinity,
};

use super::{
    CommitFailure, MergedRecordSnapshot, StorageTransactionId, VersionError, VersionResult,
    VersionedPersistence,
};
use transaction::Transaction;

/// Retention allowance for one logical session, including private history, savepoints, batches and retained views. Exhaustion is an error; this implementation does not spill.
#[derive(Debug, Clone, Copy)]
pub struct VersionedSessionOptions {
    pub retained_bytes: usize,
}

impl Default for VersionedSessionOptions {
    fn default() -> Self {
        Self {
            retained_bytes: 64 * 1024 * 1024,
        }
    }
}

/// A provider-independent session with pinned reads and conditional atomic publication. Common storage resolves typed index changes before physical admission; SQL isolation and tuple locks remain above this byte-record contract.
pub struct VersionedKeyValueStore {
    affinity: StorageSessionAffinity,
    persistence: Arc<dyn VersionedPersistence>,
    identity: Option<PersistentStorageIdentity>,
    options: VersionedSessionOptions,
    control: StorageReadControl,
    write_cancellation: uqa_core::CancellationToken,
    retained: Option<MergedRecordSnapshot>,
    active: Mutex<Option<Transaction>>,
}

impl VersionedKeyValueStore {
    /// Reclaim committed history without changing an active transaction or its retained readers. The persistence owner supplies atomic snapshot admission and physical deletion.
    pub fn reclaim_versions(&self) -> StorageBackendResult<u64> {
        self.persistence
            .reclaim_versions(&self.write_control())
            .map_err(VersionError::into_storage_error)
    }

    /// Autonomous watermark visibility follows physical reservations, including reservations made after this session pinned its record snapshot. No write capability is required and no logical transaction is started or finished.
    pub fn identifier_watermark(&self, namespace: &[u8]) -> StorageBackendResult<Option<u64>> {
        self.persistence
            .identifier_watermark(namespace, &self.control)
            .map_err(VersionError::into_storage_error)
    }

    /// Reserve identities without publishing this session's private records. Read-only sessions and unresolved sealed commits cannot allocate; rollback never reclaims a successful reservation.
    pub fn allocate_identifiers(
        &self,
        namespace: &[u8],
        request: super::IdentifierRequest,
    ) -> StorageBackendResult<super::IdentifierAllocation> {
        self.require_mutable_session()?;
        let active = self.active.lock();
        if let Some(transaction) = active.as_ref() {
            transaction
                .writable()
                .map_err(VersionError::into_storage_error)?;
        }
        self.persistence
            .allocate_identifiers(namespace, request, &self.write_control())
            .map_err(VersionError::into_storage_error)
    }

    pub fn new(
        persistence: Arc<dyn VersionedPersistence>,
        identity: Option<PersistentStorageIdentity>,
        options: VersionedSessionOptions,
    ) -> Self {
        Self::new_with_cancellation(
            persistence,
            identity,
            options,
            uqa_core::CancellationToken::new(),
        )
    }

    /// Share the invoking execution's cancellation during autonomous allocation and publication. Retained reads and rollback cleanup keep their independent control, so cancelling a statement cannot prevent its undo.
    pub fn new_with_cancellation(
        persistence: Arc<dyn VersionedPersistence>,
        identity: Option<PersistentStorageIdentity>,
        options: VersionedSessionOptions,
        write_cancellation: uqa_core::CancellationToken,
    ) -> Self {
        Self {
            affinity: StorageSessionAffinity::new(),
            persistence,
            identity,
            options,
            control: StorageReadControl::with_limit(options.retained_bytes),
            write_cancellation,
            retained: None,
            active: Mutex::new(None),
        }
    }

    /// Create an independent logical session with the same persistence and retention limit.
    pub fn new_session(&self) -> Self {
        Self::new(
            Arc::clone(&self.persistence),
            self.identity.clone(),
            self.options,
        )
    }

    /// Keep a separate transaction context and retention budget while inheriting the invoking writer's cancellation.
    pub fn new_session_with_cancellation(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) -> Self {
        Self::new_with_cancellation(
            Arc::clone(&self.persistence),
            self.identity.clone(),
            self.options,
            cancellation.clone(),
        )
    }

    /// Open an independently owned read-only session at this exact committed/private view. Its original participant and snapshot leases remain retained, but completing this reader never completes the originating transaction.
    pub fn new_retained_read_session(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<Self> {
        cancellation.check()?;
        let retained = self.view().map_err(VersionError::into_storage_error)?;
        let mut session = self.new_session_with_cancellation(cancellation);
        session.retained = Some(retained);
        Ok(session)
    }

    fn require_mutable_session(&self) -> StorageBackendResult<()> {
        if self.retained.is_some() {
            return Err(StorageBackendError::Other(
                "cannot write through a retained read-only session".into(),
            ));
        }
        Ok(())
    }

    fn write_control(&self) -> StorageReadControl {
        StorageReadControl::new(self.control.memory(), &self.write_cancellation)
    }

    pub fn options(&self) -> VersionedSessionOptions {
        self.options
    }

    /// Retain the current committed and private boundaries for a compound provider read. Later session mutations, rollback and commit do not advance this view.
    pub fn record_snapshot(&self) -> VersionResult<MergedRecordSnapshot> {
        self.view()
    }

    /// Advance an active transaction to the current committed boundary while retaining and rebasing its evaluated private changes. Retained readers keep their previous boundary; savepoint undo restores its matching private changes and committed base. SQL chooses when its isolation level allows this transition. A sealed commit cannot advance, and failed preparation leaves the active view unchanged.
    pub fn refresh_transaction_snapshot(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<()> {
        cancellation.check()?;
        let control = StorageReadControl::new(self.control.memory(), cancellation);
        let mut active = self.active.lock();
        if self.retained.is_some() {
            active.as_ref().ok_or_else(no_transaction)?;
            return Ok(());
        }
        active
            .as_mut()
            .ok_or_else(no_transaction)?
            .refresh(&*self.persistence, &control)
            .map_err(VersionError::into_storage_error)
    }

    /// Share this session's retention allowance with provider codecs and retained record readers. Creating a control clone does not create another allowance.
    pub fn retention_control(&self) -> StorageReadControl {
        self.control.clone()
    }

    pub fn session_affinity(&self) -> StorageSessionAffinity {
        self.affinity.clone()
    }

    /// Identify a sealed attempt after a failed commit. Retrying `commit_transaction` resubmits only the identical evaluated batch, and never reevaluates application code.
    pub fn pending_commit(&self) -> Option<StorageTransactionId> {
        self.active
            .lock()
            .as_ref()
            .and_then(|transaction| transaction.allocation)
    }

    /// Identify a retained physical attempt or uncertain logical completion, including an empty/read-only SSI transaction without a write receipt. Completion retries never replay evaluated application work.
    pub fn pending_transaction_completion(&self) -> Option<super::TransactionOutcomeId> {
        self.active
            .lock()
            .as_ref()
            .and_then(Transaction::pending_completion)
    }

    /// Establish the original SSI participant and fixed record boundary before this transaction's first logical access. SQL must call this at its first snapshot-bearing statement, then supply logical observations for every participating access path. Capability presence alone does not enable SQL SSI or fence legacy publishers.
    pub fn establish_serializable_snapshot(&self) -> StorageBackendResult<SerializableReadContext> {
        self.require_mutable_session()?;
        self.active
            .lock()
            .as_mut()
            .ok_or_else(no_transaction)?
            .establish_serializable(Arc::clone(&self.persistence), &self.write_control())
            .map_err(VersionError::into_storage_error)
    }

    /// Register a logical write before staging its records. SQL statement/savepoint ownership must encompass both this observation and the evaluated mutation. Retained record readers expose only read observation capability.
    pub fn observe_serializable_write(
        &self,
        predicate: super::SerializablePredicate<'_>,
    ) -> StorageBackendResult<()> {
        let mut active = self.active.lock();
        let transaction = active.as_mut().ok_or_else(no_transaction)?;
        transaction
            .writable()
            .map_err(VersionError::into_storage_error)?;
        let context = transaction.serializable_context().ok_or_else(|| {
            StorageBackendError::Other("no serializable participant in this session".into())
        })?;
        let control = self.write_control();
        context
            .with_graph(&control, |graph| {
                graph.observe_write(context.id(), predicate, &control)
            })
            .map_err(VersionError::into_storage_error)
    }

    fn begin(&self, read_only: bool) -> StorageBackendResult<()> {
        if !read_only {
            self.require_mutable_session()?;
        }
        let mut active = self.active.lock();
        if active.is_some() {
            return Err(StorageBackendError::Other(
                "a KeyValue transaction is already active".into(),
            ));
        }
        *active = Some(match self.retained.as_ref() {
            Some(view) => Transaction::at_snapshot(view.retain_committed(), true, &self.control),
            None => Transaction::new(&*self.persistence, read_only, &self.control)
                .map_err(VersionError::into_storage_error)?,
        });
        Ok(())
    }

    fn view(&self) -> VersionResult<MergedRecordSnapshot> {
        if let Some(view) = self.retained.as_ref() {
            return view.try_clone();
        }
        let active = self.active.lock();
        if let Some(transaction) = active.as_ref() {
            transaction.view()
        } else {
            Transaction::new(&*self.persistence, true, &self.control)?.view()
        }
    }

    fn write<T>(
        &self,
        operation: impl FnOnce(&mut Transaction) -> VersionResult<T>,
    ) -> StorageBackendResult<T> {
        self.require_mutable_session()?;
        let mut active = self.active.lock();
        if let Some(transaction) = active.as_mut() {
            return transaction
                .atomic(operation)
                .map_err(VersionError::into_storage_error);
        }
        let mut transaction = Transaction::new(&*self.persistence, false, &self.control)
            .map_err(VersionError::into_storage_error)?;
        let result = operation(&mut transaction).map_err(VersionError::into_storage_error)?;
        // Retain even an autocommit attempt until its durable outcome is known.
        *active = Some(transaction);
        active
            .as_mut()
            .expect("retained attempt")
            .commit(&*self.persistence, &self.write_control())?;
        *active = None;
        Ok(result)
    }

    fn savepoint_action(
        &self,
        operation: impl FnOnce(&mut Transaction) -> VersionResult<()>,
    ) -> StorageBackendResult<()> {
        let mut active = self.active.lock();
        let transaction = active.as_mut().ok_or_else(no_transaction)?;
        transaction
            .unsealed()
            .map_err(VersionError::into_storage_error)?;
        operation(transaction).map_err(VersionError::into_storage_error)
    }
}

impl super::IdentifierAllocator for VersionedKeyValueStore {
    fn identifier_watermark(&self, namespace: &[u8]) -> StorageBackendResult<Option<u64>> {
        Self::identifier_watermark(self, namespace)
    }

    fn allocate_identifiers(
        &self,
        namespace: &[u8],
        request: super::IdentifierRequest,
    ) -> StorageBackendResult<super::IdentifierAllocation> {
        Self::allocate_identifiers(self, namespace, request)
    }
}

impl KeyValueStore for VersionedKeyValueStore {
    fn serializable_session(&self) -> Option<&dyn SerializableSession> {
        self.persistence
            .serializable_coordinator()
            .map(|_| self as &dyn SerializableSession)
    }

    fn vacuum(&self) -> StorageBackendResult<()> {
        let active = self.active.lock();
        if active.is_some() {
            return Err(StorageBackendError::Other(
                "vacuum requires an inactive logical session".into(),
            ));
        }
        self.reclaim_versions().map(|_| ())
    }

    fn write_cancellation(&self) -> Option<uqa_core::CancellationToken> {
        Some(self.write_cancellation.clone())
    }

    fn open_session_with_cancellation(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
        cancellation.check()?;
        Ok(Arc::new(self.new_session_with_cancellation(cancellation)))
    }

    fn open_retained_read_session(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
        Ok(Arc::new(self.new_retained_read_session(cancellation)?))
    }

    fn transaction_model(&self) -> crate::StorageTransactionModel {
        crate::StorageTransactionModel::VersionedConcurrent {
            database: self.persistence.database_id(),
        }
    }

    fn refresh_transaction_snapshot(
        &self,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<()> {
        Self::refresh_transaction_snapshot(self, cancellation)
    }

    fn identifier_allocator(&self) -> Option<&dyn super::IdentifierAllocator> {
        Some(self)
    }

    fn with_read_view(
        &self,
        operation: &mut crate::key_value::KeyValueReadScope<'_>,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        let view = self.view().map_err(VersionError::into_storage_error)?;
        operation(&read::RecordRead {
            view: &view,
            database: self.persistence.database_id(),
            control: &self.control,
        })?;
        self.control.check()
    }

    fn with_mutation(
        &self,
        operation: &mut crate::key_value::KeyValueMutation<'_>,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        self.write(|transaction| {
            let view = transaction.view()?;
            let read = read::RecordRead {
                view: &view,
                database: self.persistence.database_id(),
                control: &self.control,
            };
            let mut batch = batch::Batch::new(self);
            operation(&read, &mut batch)?;
            self.control.check()?;
            batch.apply(transaction)
        })
    }

    fn transaction_affinity(&self) -> Option<StorageSessionAffinity> {
        Some(self.session_affinity())
    }

    fn storage_identity(&self) -> StorageBackendResult<Option<PersistentStorageIdentity>> {
        Ok(self.identity.clone())
    }

    fn open_session(&self) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
        Ok(Arc::new(self.new_session()))
    }

    fn get(&self, key: &[u8]) -> StorageBackendResult<Option<Vec<u8>>> {
        let mut value = None;
        self.visit_value(key, &self.control, &mut |bytes| {
            value = bytes.map(<[u8]>::to_vec);
            Ok(())
        })?;
        Ok(value)
    }

    fn contains_key(&self, key: &[u8]) -> StorageBackendResult<bool> {
        Ok(self
            .view()
            .and_then(|view| view.metadata(key, &self.control))
            .map_err(VersionError::into_storage_error)?
            .is_some_and(|record| record.live))
    }

    fn contains_prefix_budgeted(
        &self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        control.check()?;
        let mut found = false;
        self.view()
            .map_err(VersionError::into_storage_error)?
            .visit_keys(prefix, None, usize::MAX, control, &mut |_, record| {
                found = record.live;
                Ok(!found)
            })
            .map_err(VersionError::into_storage_error)?;
        Ok(found)
    }

    fn visit_value(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        self.view()
            .map_err(VersionError::into_storage_error)?
            .visit_value(key, control, &mut |record| {
                visit(record.and_then(|record| record.value)).map_err(VersionError::from)
            })
            .map_err(VersionError::into_storage_error)
    }

    fn visit_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        if limit == 0 {
            return Ok(());
        }
        let view = self.view().map_err(VersionError::into_storage_error)?;
        read::visit_live(&view, prefix, after, limit, control, visit)
            .map_err(VersionError::into_storage_error)
    }

    fn put(&self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.write(|transaction| transaction.replace(key, Some(value), &self.control))
    }
    fn delete(&self, key: &[u8]) -> StorageBackendResult<()> {
        self.write(|transaction| transaction.replace(key, None, &self.control))
    }

    fn scan_prefix(&self, prefix: &[u8]) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        self.scan_prefix_after(prefix, None, usize::MAX)
    }

    fn scan_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut rows = Vec::new();
        self.visit_prefix_after(prefix, after, limit, &self.control, &mut |key, value| {
            rows.push((key.to_vec(), value.to_vec()));
            Ok(())
        })?;
        Ok(rows)
    }

    fn scan_prefix_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> StorageBackendResult<Vec<Vec<u8>>> {
        let mut keys = Vec::new();
        if limit != 0 {
            self.view()
                .map_err(VersionError::into_storage_error)?
                .visit_keys(
                    prefix,
                    after,
                    usize::MAX,
                    &self.control,
                    &mut |key, record| {
                        if record.live {
                            keys.push(key.to_vec());
                        }
                        Ok(keys.len() < limit)
                    },
                )
                .map_err(VersionError::into_storage_error)?;
        }
        Ok(keys)
    }

    fn first_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
    ) -> StorageBackendResult<Option<(Vec<u8>, Vec<u8>)>> {
        Ok(self.scan_prefix_after(prefix, after, 1)?.pop())
    }
    fn delete_prefix(&self, prefix: &[u8]) -> StorageBackendResult<usize> {
        self.write(|transaction| transaction.delete_prefix(prefix, &self.control))
    }
    fn batch(&self) -> Box<dyn KeyValueBatch + '_> {
        Box::new(batch::Batch::new(self))
    }
    fn begin_transaction(&self) -> StorageBackendResult<()> {
        self.begin(false)
    }
    fn begin_read_transaction(&self) -> StorageBackendResult<()> {
        self.begin(true)
    }
    fn begin_upgradeable_transaction(&self) -> StorageBackendResult<()> {
        self.begin(self.retained.is_some())
    }
    fn in_transaction(&self) -> bool {
        self.active.lock().is_some()
    }
    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        Ok(self.active.lock().as_ref().is_some_and(|transaction| {
            transaction.changes.has_written() || transaction.has_derived_changes()
        }))
    }
    fn change_version(&self) -> StorageBackendResult<Option<u64>> {
        Ok(Some(
            self.view()
                .map_err(VersionError::into_storage_error)?
                .sequence()
                .as_u64(),
        ))
    }

    fn commit_transaction(&self) -> StorageBackendResult<()> {
        let mut active = self.active.lock();
        active
            .as_mut()
            .ok_or_else(no_transaction)?
            .commit(&*self.persistence, &self.write_control())?;
        *active = None;
        Ok(())
    }

    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        let mut active = self.active.lock();
        active
            .as_mut()
            .ok_or_else(no_transaction)?
            .abort(&*self.persistence, &self.control)?;
        *active = None;
        Ok(())
    }

    fn savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.savepoint_action(|transaction| transaction.savepoint(name, &self.control))
    }
    fn release_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.savepoint_action(|transaction| transaction.release(name))
    }
    fn rollback_to_savepoint(&self, name: &str) -> StorageBackendResult<()> {
        self.savepoint_action(|transaction| transaction.rollback_to(name, &self.control))
    }
}

fn no_transaction() -> StorageBackendError {
    StorageBackendError::Other("no active KeyValue transaction".into())
}

fn commit_error(error: CommitFailure) -> StorageBackendError {
    match error {
        CommitFailure::Rejected(error) => error.into_storage_error(),
        error @ CommitFailure::Indeterminate { .. } => StorageBackendError::backend("MVCC", error),
    }
}
