//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deferred constraint work performed at transaction boundaries.

use super::{Engine, SQLError, TransactionFrame};

impl Engine {
    pub(super) fn validate_deferred_constraints_before_commit(
        &self,
        stack: &mut Vec<TransactionFrame>,
        nested: bool,
    ) -> Result<(), SQLError> {
        if nested {
            return Ok(());
        }
        let validation = {
            let frame = stack
                .last()
                .ok_or_else(|| SQLError::Internal("COMMIT without an open transaction".into()))?;
            if frame.deferred_foreign_key_rows.is_empty() {
                return Ok(());
            }
            crate::sql::dml::validate_deferred_foreign_key_rows(
                self,
                &frame.deferred_foreign_key_rows,
            )
        };
        if let Err(validation_error) = validation {
            return Err(match self.rollback_transaction_frame(stack) {
                Ok(()) => validation_error,
                Err(rollback_error) => SQLError::Internal(format!(
                    "{validation_error}; deferred constraint rollback also failed: {rollback_error}"
                )),
            });
        }
        Ok(())
    }
}
