//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical completion does not require allocating a physical write receipt.

use super::{CommitErrorOutcome, SerializableTransactionId, StorageTransactionId};
use crate::StorageBackendError;

/// Identity retained by a session while resolving completion. A serializable participant can finish without publishing records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionOutcomeId {
    Records(StorageTransactionId),
    Serializable(SerializableTransactionId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionOutcome {
    Indeterminate(TransactionOutcomeId),
    Committed(TransactionOutcomeId),
    Aborted(TransactionOutcomeId),
}

impl TransactionOutcome {
    pub const fn id(self) -> TransactionOutcomeId {
        match self {
            Self::Indeterminate(id) | Self::Committed(id) | Self::Aborted(id) => id,
        }
    }
}

impl From<CommitErrorOutcome> for TransactionOutcome {
    fn from(outcome: CommitErrorOutcome) -> Self {
        match outcome {
            CommitErrorOutcome::Indeterminate(id) => {
                Self::Indeterminate(TransactionOutcomeId::Records(id))
            }
            CommitErrorOutcome::Committed(receipt) => {
                Self::Committed(TransactionOutcomeId::Records(receipt.transaction))
            }
            CommitErrorOutcome::Aborted(id) => Self::Aborted(TransactionOutcomeId::Records(id)),
        }
    }
}

/// Completion evidence takes precedence over a later checkpoint, cancellation or cleanup error. Keep any physical receipt alongside the logical identity for existing receipt consumers.
#[derive(Debug, thiserror::Error)]
#[error("transaction completion {outcome:?}: {source}")]
pub struct TransactionCompletionError {
    pub outcome: TransactionOutcome,
    pub publication: Option<CommitErrorOutcome>,
    #[source]
    pub source: StorageBackendError,
}

impl StorageBackendError {
    /// Inspect retained logical or physical completion through provider wrappers without parsing diagnostics. The outer completion owner has precedence over an earlier nested failure.
    pub fn transaction_outcome(&self) -> Option<TransactionOutcome> {
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(self);
        while let Some(error) = cause {
            if let Some(error) = error.downcast_ref::<TransactionCompletionError>() {
                return Some(error.outcome);
            }
            if let Some(outcome) = super::persistence::error_commit_outcome(error) {
                return Some(outcome.into());
            }
            cause = error.source();
        }
        None
    }
}
