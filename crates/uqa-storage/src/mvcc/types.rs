//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Revision identities and conditional record replacements.

/// Monotonic committed visibility boundary, independent of `PostgreSQL` XIDs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitSequence(u64);

impl CommitSequence {
    pub const INITIAL: Self = Self(0);

    /// Decode a sequence supplied by the owning durable format.
    pub const fn from_u64(value: u64) -> Self {
        Self(value)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub fn successor(self) -> VersionResult<Self> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(VersionError::SequenceExhausted)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    #[error("identifier allocation minimum exceeds maximum")]
    InvalidIdentifierBounds,
    #[error("identifier allocation space exhausted")]
    IdentifiersExhausted,
    #[error("committed sequence space exhausted")]
    SequenceExhausted,
    #[error("record commit sequence {next:?} must follow {previous:?}")]
    CommitOrder {
        previous: CommitSequence,
        next: CommitSequence,
    },
    #[error("record mutation {mutation} expected revision {expected:?}, found {actual:?}")]
    WriteConflict {
        mutation: usize,
        expected: Option<CommitSequence>,
        actual: Option<CommitSequence>,
    },
    /// The provider verified a pending receipt under exclusive admission, but evaluated derived effects used an earlier committed snapshot. Only those pure effects may be prepared again.
    #[error("derived commit snapshot {expected:?} changed to {actual:?}")]
    CommitSnapshotChanged {
        expected: CommitSequence,
        actual: CommitSequence,
    },
    #[error("record mutations {first} and {second} replace the same identity")]
    DuplicateRecord { first: usize, second: usize },
    #[error("private record revision space exhausted")]
    PrivateRevisionExhausted,
    #[error("private record savepoint {0:?} does not exist")]
    SavepointMissing(crate::StorageSavepointId),
    #[error("transaction allocation must not be zero")]
    InvalidTransactionId,
    #[error("transaction belongs to another database incarnation")]
    WrongDatabase,
    #[error("transaction allocation space exhausted")]
    TransactionIdsExhausted,
    #[error("no retained transaction allocation or outcome")]
    UnknownTransaction,
    #[error("transaction has already ended")]
    TransactionFinished,
    #[error("transaction changes are sealed for commit; resolve or roll back this transaction")]
    TransactionSealed,
    #[error("transaction already committed: {0:?}")]
    AlreadyCommitted(super::CommitReceipt),
    #[error("transaction was aborted: {0:?}")]
    AlreadyAborted(super::StorageTransactionId),
    #[error("transaction was committed with a different prepared batch")]
    CommitMismatch,
    #[error("invalid versioned record encoding: {0}")]
    InvalidEncoding(&'static str),
    #[error(transparent)]
    Memory(#[from] uqa_core::memory::MemoryError),
    #[error(transparent)]
    Cancelled(#[from] uqa_core::QueryCancelled),
    #[error(transparent)]
    Storage(#[from] crate::StorageBackendError),
}

pub type VersionResult<T> = Result<T, VersionError>;

impl VersionError {
    /// Preserve resource and provider diagnostics through existing storage APIs.
    pub fn into_storage_error(self) -> crate::StorageBackendError {
        match self {
            Self::Memory(error) => error.into(),
            Self::Cancelled(error) => error.into(),
            Self::Storage(error) => error,
            error => crate::StorageBackendError::backend("MVCC", error),
        }
    }
}

/// Already evaluated replacement of one logical record. `None` is a tombstone.
///
/// Keys must include their owning object generation. `expected == None` means no version has been observed, not that an existing tombstone can be ignored.
#[derive(Debug, Clone, Copy)]
pub struct RecordWrite<'a> {
    pub key: &'a [u8],
    pub expected: Option<CommitSequence>,
    pub value: Option<&'a [u8]>,
}
