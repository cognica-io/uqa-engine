//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic persistence and durable transaction outcomes, independent of physical drivers.

use std::sync::Arc;

use uqa_core::memory::BudgetedVec;

use crate::read_control::StorageReadControl;
use crate::StorageBackendError;

use super::{
    CommitSequence, CommittedRecordSnapshot, PreparedRecordCommit, ScannedRecord, VersionError,
    VersionResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DatabaseId([u8; 16]);

impl DatabaseId {
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

/// A durable, non-reused allocation in one database incarnation; it is not a SQL-visible XID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StorageTransactionId {
    database: DatabaseId,
    allocation: u64,
}

impl StorageTransactionId {
    pub fn new(database: DatabaseId, allocation: u64) -> VersionResult<Self> {
        if allocation == 0 {
            return Err(VersionError::InvalidTransactionId);
        }
        Ok(Self {
            database,
            allocation,
        })
    }
    pub const fn database(self) -> DatabaseId {
        self.database
    }
    pub const fn allocation(self) -> u64 {
        self.allocation
    }
}

pub type CommitFingerprint = [u8; 32];
pub type RecordPage = BudgetedVec<ScannedRecord>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitReceipt {
    pub transaction: StorageTransactionId,
    pub sequence: CommitSequence,
    pub fingerprint: CommitFingerprint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitStatus {
    /// No retained authoritative outcome exists. This is not proof of rollback.
    Unknown,
    Pending,
    Aborted,
    Committed(CommitReceipt),
}

/// Durable outcome information retained in a storage error, including through provider wrappers. Absence of this information does not prove that a transaction aborted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitErrorOutcome {
    Indeterminate(StorageTransactionId),
    Committed(CommitReceipt),
    Aborted(StorageTransactionId),
}

impl StorageBackendError {
    /// Classify typed commit evidence without interpreting provider diagnostic text.
    pub fn commit_outcome(&self) -> Option<CommitErrorOutcome> {
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(self);
        while let Some(error) = cause {
            if let Some(CommitFailure::Indeterminate { transaction, .. }) =
                error.downcast_ref::<CommitFailure>()
            {
                return Some(CommitErrorOutcome::Indeterminate(*transaction));
            }
            if let Some(VersionError::AlreadyCommitted(receipt)) =
                error.downcast_ref::<VersionError>()
            {
                return Some(CommitErrorOutcome::Committed(*receipt));
            }
            if let Some(VersionError::AlreadyAborted(transaction)) =
                error.downcast_ref::<VersionError>()
            {
                return Some(CommitErrorOutcome::Aborted(*transaction));
            }
            cause = error.source();
        }
        None
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CommitFailure {
    /// The attempt published no changes; any prior successful attempt still has its receipt.
    #[error(transparent)]
    Rejected(#[from] VersionError),
    #[error("commit outcome for {transaction:?} is indeterminate: {source}")]
    Indeterminate {
        transaction: StorageTransactionId,
        #[source]
        source: StorageBackendError,
    },
}

pub type CommitResult = Result<CommitReceipt, CommitFailure>;

/// Versioned record persistence. Every mutation of the current sequence, records and receipt must share one physical commit. Implementations retain no physical writer between calls and must distinguish rejection from an uncertain native commit outcome.
pub trait VersionedPersistence: Send + Sync {
    fn database_id(&self) -> DatabaseId;

    fn graph_record_layout(&self) -> Option<&dyn super::GraphRecordLayout> {
        None
    }

    /// Common Key/Value addressing is the default. Native record adapters and their wrappers must return their own physical occurrence layout.
    fn occurrence_record_layout(&self) -> &dyn super::OccurrenceRecordLayout {
        &crate::key_value::KeyValueOccurrenceRecords
    }

    /// Persist a new transaction allocation before returning it, without advancing record visibility.
    fn allocate_transaction(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<StorageTransactionId>;

    fn snapshot(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Arc<dyn CommittedRecordSnapshot>>;

    /// Retry the same sealed logical changes and fingerprint. Return a durable matching receipt before validating snapshots or record heads. After verifying a pending receipt under exclusive admission, call `PreparedRecordCommit::validate_snapshot` before validating heads; only its rejection permits common storage to materialize derived effects again. Canonical changes and application callbacks must never be replayed, and mismatched fingerprint reuse is rejected.
    fn commit(
        &self,
        transaction: StorageTransactionId,
        prepared: &PreparedRecordCommit,
        control: &StorageReadControl,
    ) -> CommitResult;

    fn commit_status(
        &self,
        transaction: StorageTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<CommitStatus>;

    /// Atomically end a pending allocation. Return an existing committed receipt unchanged; abort must never overwrite a commit.
    fn abort(
        &self,
        transaction: StorageTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<CommitStatus>;
}

/// Resolve an idempotent commit under the provider's exclusive commit boundary before validating or changing records.
pub fn resolve_prepared_receipt(
    status: CommitStatus,
    transaction: StorageTransactionId,
    fingerprint: CommitFingerprint,
) -> VersionResult<Option<CommitReceipt>> {
    match status {
        CommitStatus::Pending => Ok(None),
        CommitStatus::Committed(receipt)
            if receipt.transaction == transaction && receipt.fingerprint == fingerprint =>
        {
            Ok(Some(receipt))
        }
        CommitStatus::Committed(_) => Err(VersionError::CommitMismatch),
        CommitStatus::Aborted => Err(VersionError::TransactionFinished),
        CommitStatus::Unknown => Err(VersionError::UnknownTransaction),
    }
}
