//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Request completion from the transaction owner's actual publication and rollback boundaries.

use super::{Engine, TransactionStatus};

/// Transaction facts for one admitted host operation. These facts never authorize replay: a message can contain explicit commits, and nontransactional sequence effects survive rollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionCompletion {
    /// No transaction completed during a failed operation.
    None,
    /// The operation completed its implicit work, or a commit was confirmed before a later failure.
    Committed,
    /// The transaction owner confirmed rollback without an intervening commit.
    RolledBack,
    /// An unresolved physical or logical completion remains in the session.
    Pending,
    /// An explicit or nested transaction remains open, possibly failed.
    InTransaction,
    /// The operation crossed both commit and rollback boundaries, or cleanup could not establish one outcome.
    Unknown,
}

/// The original native return value or error together with its operation-scoped transaction facts.
#[derive(Debug)]
pub struct ExecutionOutcome<T, E> {
    pub result: Result<T, E>,
    pub completion: ExecutionCompletion,
}

/// Constant-size, nontransactional facts. Snapshot/rollback must never rewind completed publication. Saturation deliberately loses precision instead of wrapping into a false absence of events.
#[derive(Clone, Copy, Default)]
pub(crate) struct CompletionCounters {
    committed: u64,
    rolled_back: u64,
    unknown: u64,
}

impl CompletionCounters {
    fn completion_since(self, before: Self, success: bool) -> ExecutionCompletion {
        if [self.committed, self.rolled_back, self.unknown].contains(&u64::MAX)
            || self.unknown != before.unknown
        {
            return ExecutionCompletion::Unknown;
        }
        match (
            self.committed != before.committed,
            self.rolled_back != before.rolled_back,
        ) {
            (true, true) => ExecutionCompletion::Unknown,
            (true, false) => ExecutionCompletion::Committed,
            (false, true) => ExecutionCompletion::RolledBack,
            (false, false) if success => ExecutionCompletion::Committed,
            (false, false) => ExecutionCompletion::None,
        }
    }
}

impl Engine {
    /// Run one host operation under the session statement gate and report its native completion facts. The operation retains its own admission, execution and recovery owner; this adapter neither starts a transaction nor retries work. Nested native calls contribute to the same interval, while earlier calls and sibling sessions do not. A successful operation without a retained transaction has finished its implicit work, including a read-only evaluation that required no physical transaction.
    pub fn execute_with_completion<T, E>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, E>,
    ) -> ExecutionOutcome<T, E> {
        let _statement = self.lock_statement_gate();
        let before = *self.session.completion_counters.lock();
        let result = operation(self);
        let stack = self.session.transactions.lock();
        let completion = match stack.last().map(|frame| frame.status) {
            Some(TransactionStatus::CommitPending(_) | TransactionStatus::RollbackPending(_)) => {
                ExecutionCompletion::Pending
            }
            Some(_) => ExecutionCompletion::InTransaction,
            None => self
                .session
                .completion_counters
                .lock()
                .completion_since(before, result.is_ok()),
        };
        ExecutionOutcome { result, completion }
    }

    pub(super) fn record_transaction_completion(&self, completion: ExecutionCompletion) {
        let mut counters = self.session.completion_counters.lock();
        let counter = match completion {
            ExecutionCompletion::Committed => &mut counters.committed,
            ExecutionCompletion::RolledBack => &mut counters.rolled_back,
            ExecutionCompletion::Unknown => &mut counters.unknown,
            _ => unreachable!("only a completed outer transaction publishes an outcome"),
        };
        *counter = counter.saturating_add(1);
    }
}

#[cfg(test)]
mod tests;
