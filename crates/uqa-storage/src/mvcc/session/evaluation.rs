//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Failed evaluation resolves any early writer allocation without replaying application code.

use super::{
    batch, read, transaction::Transaction, SerializableReadContext, VersionedKeyValueStore,
};
use crate::key_value::{KeyValueMutation, KeyValueReadRevision};
use crate::mvcc::{CommitSequence, VersionError, VersionResult};
use crate::StorageBackendResult;

impl VersionedKeyValueStore {
    pub(super) fn evaluate_mutation(
        &self,
        operation: &mut KeyValueMutation<'_>,
        prefixes: Option<&[&[u8]]>,
    ) -> StorageBackendResult<Option<KeyValueReadRevision>> {
        self.control.check()?;
        let ((identity, sequence), receipt) = self.write_with_receipt(|transaction| {
            self.stage_record_mutation(transaction, operation, prefixes)
        })?;
        Ok(match (identity, receipt) {
            // The immediately following commit cannot have rebased over a peer. This receipt was captured under the same session mutex, never from a later read.
            (Some(_), Some(receipt)) if sequence.successor().ok() == Some(receipt.sequence) => {
                Some(KeyValueReadRevision::records(
                    self.persistence.database_id(),
                    receipt.sequence,
                    None,
                ))
            }
            (identity, _) => identity,
        })
    }

    fn stage_record_mutation(
        &self,
        transaction: &mut Transaction,
        operation: &mut KeyValueMutation<'_>,
        prefixes: Option<&[&[u8]]>,
    ) -> VersionResult<(Option<KeyValueReadRevision>, CommitSequence)> {
        let view = transaction.view()?;
        let read = read::RecordRead {
            view: &view,
            database: self.persistence.database_id(),
            control: &self.control,
        };
        let mut batch = batch::Batch::new(
            self,
            transaction
                .serializable_context()
                .map(SerializableReadContext::id),
        );
        operation(&read, &mut batch)?;
        self.control.check()?;
        batch.apply(transaction)?;
        // Cache certification is optional and never changes the mutation outcome. The selected root is captured while this transaction is still exclusively borrowed.
        let identity = prefixes.and_then(|prefixes| {
            let staged = transaction.view().ok()?;
            crate::key_value::KeyValueRead::revision(
                &read::RecordRead {
                    view: &staged,
                    database: self.persistence.database_id(),
                    control: &self.control,
                },
                prefixes,
            )
            .ok()
        });
        Ok((identity, view.sequence()))
    }

    pub(super) fn try_transaction_mutation(
        &self,
        operation: &mut KeyValueMutation<'_>,
    ) -> StorageBackendResult<bool> {
        self.control.check()?;
        self.require_mutable_session()?;
        let mut active = self.active.lock();
        let Some(transaction) = active.as_mut() else {
            return Ok(false);
        };
        transaction
            .atomic(|tx| self.stage_record_mutation(tx, operation, None))
            .map_err(VersionError::into_storage_error)?;
        Ok(true)
    }

    pub(super) fn evaluate_autocommit<T>(
        &self,
        active: &mut Option<Transaction>,
        mut transaction: Transaction,
        operation: impl FnOnce(&mut Transaction) -> VersionResult<T>,
    ) -> StorageBackendResult<T> {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(&mut transaction)));
        match result {
            Ok(Ok(value)) => {
                // Both ordinary publication and early origin allocation retain their exact attempt through completion.
                *active = Some(transaction);
                Ok(value)
            }
            failure => {
                let cleanup = if transaction.allocation.is_some() {
                    transaction.require_evaluation_abort();
                    *active = Some(transaction);
                    self.abort_evaluation(active)
                } else {
                    Ok(())
                };
                match failure {
                    Ok(Err(error)) => {
                        cleanup?;
                        Err(error.into_storage_error())
                    }
                    Err(payload) => std::panic::resume_unwind(payload),
                    Ok(Ok(_)) => unreachable!("successful evaluation handled above"),
                }
            }
        }
    }

    fn abort_evaluation(&self, active: &mut Option<Transaction>) -> StorageBackendResult<()> {
        let transaction = active.as_mut().ok_or_else(|| {
            VersionError::InvalidEncoding("missing evaluated transaction").into_storage_error()
        })?;
        // Use the original cleanup allowance, even if the invoking operation cancelled its write control.
        let cleanup = crate::read_control::StorageReadControl::new(
            self.control.memory(),
            &uqa_core::CancellationToken::new(),
        );
        transaction.abort(&*self.persistence, &cleanup)?;
        transaction.acknowledge_completion(&*self.persistence, &cleanup)?;
        *active = None;
        Ok(())
    }
}
