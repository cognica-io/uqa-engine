//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation provenance reserves the actual publication identity before evaluated bytes escape.

use super::Transaction;
use crate::mvcc::{
    StorageMutationOrigin, StorageTransactionId, VersionError, VersionResult, VersionedPersistence,
};
use crate::read_control::StorageReadControl;

impl Transaction {
    pub(in crate::mvcc::session) fn mutation_origin(
        &mut self,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> VersionResult<StorageMutationOrigin> {
        self.writable()?;
        control.check()?;
        let revision = self
            .mutation_revision
            .checked_add(1)
            .ok_or(VersionError::InvalidEncoding("mutation revision exhausted"))?;
        let transaction = self.ensure_allocation(persistence, control)?;
        // Savepoint and statement rollback restore values, never this allocation watermark.
        self.mutation_revision = revision;
        Ok(StorageMutationOrigin::new(transaction, revision))
    }

    pub(super) fn ensure_allocation(
        &mut self,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> VersionResult<StorageTransactionId> {
        if let Some(transaction) = self.allocation {
            return Ok(transaction);
        }
        let owner = match persistence.allocate_managed_transaction(control) {
            Err(VersionError::ReceiptRetentionExhausted { .. }) => {
                persistence.reclaim_transaction_receipts(control)?;
                persistence.allocate_managed_transaction(control)?
            }
            result => result?,
        };
        let transaction = owner.transaction();
        self.allocation = Some(transaction);
        self.receipt_owner = Some(owner);
        Ok(transaction)
    }

    pub(in crate::mvcc::session) fn pending_commit(&self) -> Option<StorageTransactionId> {
        if self.prepared.is_some() || self.outcome.is_some() || self.abort_only {
            self.allocation
        } else {
            None
        }
    }

    pub(in crate::mvcc::session) fn require_evaluation_abort(&mut self) {
        self.abort_only = true;
    }
}
