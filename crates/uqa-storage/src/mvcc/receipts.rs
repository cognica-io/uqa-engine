//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit terminal receipt ownership and database-wide admission bounds.

use super::{
    CommitReceipt, CommitStatus, SerializableParticipant, SerializableTransactionId,
    StorageTransactionId, VersionError, VersionResult,
};

/// Default maximum number of durable receipt entries, including pending allocations. Providers persist this database-wide limit and expose configuration independently of session byte allowances. A receipt has a fixed-size identity, outcome and ownership flag; physical page and index overhead remain provider-owned.
pub const DEFAULT_RECEIPT_RETENTION_LIMIT: u64 = 65_536;

/// A separate liveness namespace for physical receipt owners, reusing the common retained-participant transport without admitting an SSI transaction.
pub fn receipt_lease_id(transaction: StorageTransactionId) -> SerializableTransactionId {
    SerializableTransactionId::new(
        transaction.database(),
        *b"UQAReceiptLease1",
        transaction.allocation(),
    )
    .expect("receipt allocation and namespace are nonzero")
}

/// Keeps a managed allocation's resolution owner alive through publication and acknowledgement retries. Dropping the final lease permits provider recovery, but never establishes a physical outcome by itself. The conservative untracked form preserves legacy providers' manually retained receipt semantics.
pub struct RetainedTransactionAllocation {
    transaction: StorageTransactionId,
    _lease: Option<SerializableParticipant>,
}

impl RetainedTransactionAllocation {
    pub fn untracked(transaction: StorageTransactionId) -> Self {
        Self {
            transaction,
            _lease: None,
        }
    }

    pub fn retain(
        transaction: StorageTransactionId,
        lease: SerializableParticipant,
    ) -> VersionResult<Self> {
        if lease.id() != receipt_lease_id(transaction) {
            return Err(VersionError::InvalidEncoding(
                "receipt owner lease identity mismatch",
            ));
        }
        Ok(Self {
            transaction,
            _lease: Some(lease),
        })
    }

    pub fn transaction(&self) -> StorageTransactionId {
        self.transaction
    }
}

/// Release the caller's right to resolve one confirmed physical outcome. After successful acknowledgement, lookup or replay may return Unknown; acknowledgement itself remains idempotent. Providers retain every SSI publication reference independently and must never infer acknowledgement from age or a snapshot horizon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptAcknowledgement {
    Committed(CommitReceipt),
    Aborted(StorageTransactionId),
}

impl ReceiptAcknowledgement {
    pub fn transaction(self) -> StorageTransactionId {
        match self {
            Self::Committed(receipt) => receipt.transaction,
            Self::Aborted(transaction) => transaction,
        }
    }

    /// A missing previously allocated receipt is an idempotent acknowledgement retry. Providers must first verify the database incarnation and allocation watermark. Pending or changed outcomes cannot be released.
    pub fn validate(self, status: CommitStatus) -> VersionResult<()> {
        match (self, status) {
            (_, CommitStatus::Unknown) | (Self::Aborted(_), CommitStatus::Aborted) => Ok(()),
            (Self::Committed(expected), CommitStatus::Committed(actual)) if expected == actual => {
                Ok(())
            }
            (_, CommitStatus::Pending) => Err(VersionError::TransactionSealed),
            (_, CommitStatus::Committed(receipt)) => Err(VersionError::AlreadyCommitted(receipt)),
            (_, CommitStatus::Aborted) => Err(VersionError::AlreadyAborted(self.transaction())),
        }
    }
}
