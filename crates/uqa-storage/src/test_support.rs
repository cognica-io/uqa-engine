//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scoped failures at the actual record publication boundary, without replacing providers.

use std::cell::Cell;

use crate::{
    mvcc::{
        CommitFailure, CommitResult, CommitStatus, StorageTransactionId, VersionError,
        VersionResult,
    },
    StorageBackendError,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordOperation {
    Commit,
    Abort,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordBoundary {
    Before,
    After,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordEvent {
    pub transaction: StorageTransactionId,
    pub operation: RecordOperation,
    pub boundary: RecordBoundary,
}

pub enum RecordFault {
    /// Reject before physical execution, preserving the chosen native error.
    Reject(VersionError),
    /// Hide whether physical execution completed, preserving the original transaction identity.
    Unknown(StorageBackendError),
}

impl RecordFault {
    pub fn unknown(message: impl Into<String>) -> Self {
        Self::Unknown(StorageBackendError::Other(message.into()))
    }

    fn commit_failure(self, event: RecordEvent) -> CommitFailure {
        let source = match self {
            Self::Reject(error) if event.boundary == RecordBoundary::Before => {
                return CommitFailure::Rejected(error);
            }
            Self::Reject(error) => error.into_storage_error(),
            Self::Unknown(error) => error,
        };
        CommitFailure::Indeterminate {
            transaction: event.transaction,
            source,
        }
    }

    fn abort_failure(self, event: RecordEvent) -> VersionError {
        match self.commit_failure(event) {
            CommitFailure::Rejected(error) => error,
            error @ CommitFailure::Indeterminate { .. } => {
                StorageBackendError::backend("record completion", error).into()
            }
        }
    }
}

type RecordObserver = fn(RecordEvent) -> Option<RecordFault>;

thread_local! {
    static OBSERVER: Cell<Option<RecordObserver>> = const { Cell::new(None) };
}

/// Observe record calls on this thread. Nested scopes and unwinding restore the original observer; independent workers and provider sessions are never implicitly selected.
pub fn with_record_faults<T>(observer: RecordObserver, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<RecordObserver>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OBSERVER.with(|observer| observer.set(self.0));
        }
    }
    let restore = Restore(OBSERVER.with(|slot| slot.replace(Some(observer))));
    let result = operation();
    drop(restore);
    result
}

fn observe(event: RecordEvent) -> Option<RecordFault> {
    OBSERVER.with(|observer| observer.get().and_then(|observer| observer(event)))
}

pub(crate) fn commit(
    transaction: StorageTransactionId,
    operation: impl FnOnce() -> CommitResult,
) -> CommitResult {
    let mut event = RecordEvent {
        transaction,
        operation: RecordOperation::Commit,
        boundary: RecordBoundary::Before,
    };
    if let Some(fault) = observe(event) {
        return Err(fault.commit_failure(event));
    }
    let receipt = operation()?;
    event.boundary = RecordBoundary::After;
    if let Some(fault) = observe(event) {
        return Err(fault.commit_failure(event));
    }
    Ok(receipt)
}

pub(crate) fn abort(
    transaction: StorageTransactionId,
    operation: impl FnOnce() -> VersionResult<CommitStatus>,
) -> VersionResult<CommitStatus> {
    let mut event = RecordEvent {
        transaction,
        operation: RecordOperation::Abort,
        boundary: RecordBoundary::Before,
    };
    if let Some(fault) = observe(event) {
        return Err(fault.abort_failure(event));
    }
    let status = operation()?;
    event.boundary = RecordBoundary::After;
    if let Some(fault) = observe(event) {
        return Err(fault.abort_failure(event));
    }
    Ok(status)
}

#[cfg(test)]
mod tests;
