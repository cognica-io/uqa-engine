//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_core::memory::BudgetedVec;

use crate::mvcc::key::RecordKey;
use crate::mvcc::{
    CommitFailure, CommitStatus, CommittedRecordSnapshot, MergedRecordSnapshot,
    PreparedRecordCommit, PrivateRecordChanges, RecordWrite, StorageTransactionId, VersionError,
    VersionResult, VersionedPersistence,
};
use crate::read_control::StorageReadControl;
use crate::{StorageBackendError, StorageSavepointId};

struct Savepoint {
    name: BudgetedVec<u8>,
    id: StorageSavepointId,
}

pub(super) struct Transaction {
    committed: Arc<dyn CommittedRecordSnapshot>,
    pub(super) changes: PrivateRecordChanges,
    read_only: bool,
    pub(super) allocation: Option<StorageTransactionId>,
    prepared: Option<PreparedRecordCommit>,
    savepoints: BudgetedVec<Savepoint>,
}

impl Transaction {
    pub(super) fn new(
        persistence: &dyn VersionedPersistence,
        read_only: bool,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        Ok(Self {
            committed: persistence.snapshot(control)?,
            changes: PrivateRecordChanges::new(control.memory()),
            read_only,
            allocation: None,
            prepared: None,
            savepoints: BudgetedVec::new(control.memory()),
        })
    }

    pub(super) fn view(&self) -> VersionResult<MergedRecordSnapshot> {
        Ok(MergedRecordSnapshot::new(
            Arc::clone(&self.committed),
            self.changes.snapshot()?,
        ))
    }

    pub(super) fn writable(&self) -> VersionResult<()> {
        if self.read_only {
            return Err(StorageBackendError::Other(
                "cannot write in a read-only KeyValue transaction".into(),
            )
            .into());
        }
        if self.prepared.is_some() {
            return Err(VersionError::TransactionSealed);
        }
        Ok(())
    }

    pub(super) fn replace(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.writable()?;
        let record = self.view()?.metadata(key, control)?;
        let expected = record.and_then(|record| record.revision);
        let exists = record.is_some_and(|record| record.live);
        if value.is_none() && !exists {
            return Ok(());
        }
        self.changes.apply(
            &[RecordWrite {
                key,
                expected,
                value,
            }],
            control,
        )
    }

    pub(super) fn delete_prefix(
        &mut self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<usize> {
        self.writable()?;
        let mut keys = BudgetedVec::new(control.memory());
        self.view()?
            .visit_keys(prefix, None, usize::MAX, control, &mut |key, record| {
                if record.live {
                    keys.push((RecordKey::new(key, control.memory())?, record.revision))?;
                }
                Ok(true)
            })?;
        for (key, expected) in keys.iter() {
            self.changes.apply(
                &[RecordWrite {
                    key: key.bytes(),
                    expected: *expected,
                    value: None,
                }],
                control,
            )?;
        }
        Ok(keys.len())
    }

    pub(super) fn atomic<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> VersionResult<T>,
    ) -> VersionResult<T> {
        self.writable()?;
        let id = StorageSavepointId::allocate();
        self.changes.savepoint(id)?;
        let result = operation(self);
        if result.is_err() {
            self.changes.rollback_to_savepoint(id)?;
        }
        self.changes.release_savepoint(id)?;
        result
    }

    pub(super) fn savepoint(
        &mut self,
        name: &str,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        let mut owned = BudgetedVec::new(control.memory());
        owned.extend_from_slice(name.as_bytes())?;
        self.savepoints.reserve(1)?;
        let id = StorageSavepointId::allocate();
        self.changes.savepoint(id)?;
        self.savepoints.push(Savepoint { name: owned, id })?;
        Ok(())
    }

    fn savepoint_position(&self, name: &str) -> VersionResult<usize> {
        self.savepoints
            .iter()
            .rposition(|savepoint| &*savepoint.name == name.as_bytes())
            .ok_or_else(|| {
                StorageBackendError::Other(format!("unknown KeyValue savepoint `{name}`")).into()
            })
    }

    pub(super) fn release(&mut self, name: &str) -> VersionResult<()> {
        let position = self.savepoint_position(name)?;
        self.changes
            .release_savepoint(self.savepoints[position].id)?;
        self.savepoints.truncate(position);
        Ok(())
    }

    pub(super) fn rollback_to(&mut self, name: &str) -> VersionResult<()> {
        let position = self.savepoint_position(name)?;
        self.changes
            .rollback_to_savepoint(self.savepoints[position].id)?;
        self.savepoints.truncate(position + 1);
        Ok(())
    }

    pub(super) fn commit(
        &mut self,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> Result<(), CommitFailure> {
        if self.prepared.is_none() {
            self.prepared = Some(self.changes.prepare(control)?);
        }
        let prepared = self.prepared.as_ref().expect("prepared once");
        if prepared.records().is_empty() {
            return Ok(());
        }
        let allocation = if let Some(id) = self.allocation {
            id
        } else {
            let id = persistence.allocate_transaction(control)?;
            self.allocation = Some(id);
            id
        };
        persistence.commit(allocation, prepared, control)?;
        Ok(())
    }

    pub(super) fn abort(
        &self,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        let Some(id) = self.allocation else {
            return Ok(());
        };
        match persistence.abort(id, control)? {
            CommitStatus::Aborted => Ok(()),
            CommitStatus::Committed(receipt) => Err(VersionError::AlreadyCommitted(receipt)),
            CommitStatus::Unknown | CommitStatus::Pending => Err(VersionError::UnknownTransaction),
        }
    }
}
