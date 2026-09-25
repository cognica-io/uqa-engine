//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Failed evaluation resolves any early writer allocation without replaying application code.

use super::{transaction::Transaction, VersionedKeyValueStore};
use crate::mvcc::{VersionError, VersionResult};
use crate::StorageBackendResult;

impl VersionedKeyValueStore {
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
