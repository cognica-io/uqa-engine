//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit terminal receipt ownership and database-wide admission bounds.

use super::{
    CommitReceipt, CommitStatus, DatabaseId, SerializableGraph, SerializableLeases,
    SerializableParticipant, SerializableTransactionId, StorageTransactionId, VersionError,
    VersionResult,
};
use crate::read_control::StorageReadControl;
use uqa_core::memory::BudgetedVec;

/// Default maximum number of durable receipt entries, including pending allocations. Providers persist this database-wide limit and expose configuration independently of session byte allowances. A receipt has a fixed-size identity, outcome and ownership flag; physical page and index overhead remain provider-owned.
pub const DEFAULT_RECEIPT_RETENTION_LIMIT: u64 = 65_536;

/// A durable change selected under both SSI and receipt lease admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiptReclamation {
    Remove,
    AcknowledgeAborted,
    AcknowledgeCommitted,
}

/// Collect at most 256 changes while the provider's receipt cursor is open. Apply them in the same transaction after releasing the cursor.
pub struct ReceiptReclamationBatch<'a> {
    database: DatabaseId,
    allocated: u64,
    graph: &'a SerializableGraph,
    leases: &'a dyn SerializableLeases,
    control: &'a StorageReadControl,
    selected: BudgetedVec<([u8; 8], ReceiptReclamation)>,
}

impl<'a> ReceiptReclamationBatch<'a> {
    const LIMIT: usize = 256;

    pub fn new(
        database: DatabaseId,
        allocated: u64,
        graph: &'a SerializableGraph,
        leases: &'a dyn SerializableLeases,
        control: &'a StorageReadControl,
    ) -> Self {
        Self {
            database,
            allocated,
            graph,
            leases,
            control,
            selected: BudgetedVec::new(control.memory()),
        }
    }

    pub fn reserve_page(&mut self) -> VersionResult<()> {
        self.selected.reserve(Self::LIMIT)?;
        Ok(())
    }

    pub fn transaction(&self, allocation: u64) -> VersionResult<StorageTransactionId> {
        if allocation > self.allocated {
            return Err(VersionError::InvalidEncoding(
                "receipt exceeds allocation watermark",
            ));
        }
        StorageTransactionId::new(self.database, allocation)
    }

    /// Read the physical outcome only for a selected entry. An abandoned lease gives permission to release a receipt while its stored bytes determine the outcome.
    pub fn visit<E: From<VersionError>>(
        &mut self,
        id: StorageTransactionId,
        acknowledged: bool,
        managed: impl FnOnce() -> Result<bool, E>,
        outcome: impl FnOnce() -> Result<CommitStatus, E>,
    ) -> Result<bool, E> {
        if self.selected.len() == Self::LIMIT {
            return Ok(false);
        }
        let referenced = self.graph.retains_transaction_receipt(id);
        if acknowledged && referenced {
            return Ok(true);
        }
        let managed = managed()?;
        if !acknowledged
            && (!managed || self.leases.is_alive(receipt_lease_id(id), self.control)?)
        {
            return Ok(true);
        }
        let terminal = match outcome()? {
            CommitStatus::Unknown => return Err(VersionError::UnknownTransaction.into()),
            CommitStatus::Committed(_) => ReceiptReclamation::AcknowledgeCommitted,
            CommitStatus::Pending | CommitStatus::Aborted => ReceiptReclamation::AcknowledgeAborted,
        };
        let action = if referenced {
            terminal
        } else {
            ReceiptReclamation::Remove
        };
        self.selected
            .push((id.allocation().to_be_bytes(), action))
            .map_err(VersionError::from)?;
        Ok(self.selected.len() < Self::LIMIT)
    }

    pub fn entries(&self) -> impl ExactSizeIterator<Item = (u64, ReceiptReclamation)> + '_ {
        self.selected
            .iter()
            .map(|(allocation, action)| (u64::from_be_bytes(*allocation), *action))
    }
}

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
    lease: Option<SerializableParticipant>,
}

impl RetainedTransactionAllocation {
    pub fn untracked(transaction: StorageTransactionId) -> Self {
        Self {
            transaction,
            lease: None,
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
            lease: Some(lease),
        })
    }

    pub fn transaction(&self) -> StorageTransactionId {
        self.transaction
    }

    /// Whether this owner retains a resolution lease. Providers must also verify their durable managed-owner flag before using lease death to reclaim a receipt.
    pub fn has_resolution_lease(&self) -> bool {
        self.lease.is_some()
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
