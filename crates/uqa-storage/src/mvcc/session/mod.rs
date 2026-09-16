//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical Key/Value sessions retain private changes rather than a physical writer.

mod batch;
mod read;
mod transaction;

use std::sync::Arc;

use parking_lot::Mutex;

use crate::read_control::{KeyValueReadVisitor, StorageReadControl, ValueReadVisitor};
use crate::{
    KeyValueBatch, KeyValueStore, PersistentStorageIdentity, StorageBackendError,
    StorageBackendResult,
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

/// A provider-independent session with pinned reads and conditional atomic publication. SQL isolation, locks and index merging belong to the callers above this byte-record contract.
pub struct VersionedKeyValueStore {
    persistence: Arc<dyn VersionedPersistence>,
    identity: Option<PersistentStorageIdentity>,
    options: VersionedSessionOptions,
    control: StorageReadControl,
    active: Mutex<Option<Transaction>>,
}

impl VersionedKeyValueStore {
    pub fn new(
        persistence: Arc<dyn VersionedPersistence>,
        identity: Option<PersistentStorageIdentity>,
        options: VersionedSessionOptions,
    ) -> Self {
        Self {
            persistence,
            identity,
            options,
            control: StorageReadControl::with_limit(options.retained_bytes),
            active: Mutex::new(None),
        }
    }

    /// Identify a sealed attempt after a failed commit. Retrying `commit_transaction` resubmits only the identical evaluated batch, and never reevaluates application code.
    pub fn pending_commit(&self) -> Option<StorageTransactionId> {
        self.active
            .lock()
            .as_ref()
            .and_then(|transaction| transaction.allocation)
    }

    fn begin(&self, read_only: bool) -> StorageBackendResult<()> {
        let mut active = self.active.lock();
        if active.is_some() {
            return Err(StorageBackendError::Other(
                "a KeyValue transaction is already active".into(),
            ));
        }
        *active = Some(
            Transaction::new(&*self.persistence, read_only, &self.control)
                .map_err(VersionError::into_storage_error)?,
        );
        Ok(())
    }

    fn view(&self) -> VersionResult<MergedRecordSnapshot> {
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
            .commit(&*self.persistence, &self.control)
            .map_err(commit_error)?;
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
            .writable()
            .map_err(VersionError::into_storage_error)?;
        operation(transaction).map_err(VersionError::into_storage_error)
    }
}

impl KeyValueStore for VersionedKeyValueStore {
    fn storage_identity(&self) -> StorageBackendResult<Option<PersistentStorageIdentity>> {
        Ok(self.identity.clone())
    }

    fn open_session(&self) -> StorageBackendResult<Arc<dyn KeyValueStore>> {
        Ok(Arc::new(Self::new(
            Arc::clone(&self.persistence),
            self.identity.clone(),
            self.options,
        )))
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
        let mut found = false;
        self.visit_value(key, &self.control, &mut |bytes| {
            found = bytes.is_some();
            Ok(())
        })?;
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
        self.visit_prefix_after(prefix, after, limit, &self.control, &mut |key, _| {
            keys.push(key.to_vec());
            Ok(())
        })?;
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
    fn in_transaction(&self) -> bool {
        self.active.lock().is_some()
    }
    fn transaction_has_written(&self) -> StorageBackendResult<bool> {
        Ok(self
            .active
            .lock()
            .as_ref()
            .is_some_and(|transaction| transaction.changes.has_written()))
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
            .commit(&*self.persistence, &self.control)
            .map_err(commit_error)?;
        *active = None;
        Ok(())
    }

    fn rollback_transaction(&self) -> StorageBackendResult<()> {
        let mut active = self.active.lock();
        active
            .as_ref()
            .ok_or_else(no_transaction)?
            .abort(&*self.persistence, &self.control)
            .map_err(VersionError::into_storage_error)?;
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
        self.savepoint_action(|transaction| transaction.rollback_to(name))
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
