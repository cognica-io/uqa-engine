//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The cancellation of the statement the current thread runs. Work inside one scalar evaluation that can run long checks it as `PostgreSQL`'s `CHECK_FOR_INTERRUPTS` observes a cancel request or `statement_timeout`, without receiving a production control from its caller.

use std::{cell::RefCell, marker::PhantomData, rc::Rc};
use uqa_core::{CancellationToken, QueryCancelled};

thread_local! {
    /// The cancellation token of the statement the current thread runs while a [`StatementCancellationScope`] is entered.
    static STATEMENT_CANCELLATION: RefCell<Option<CancellationToken>> = const { RefCell::new(None) };
}

/// The running statement's cancellation token, which a thread dispatching part of the statement to a worker captures for the worker to enter; `None` outside any statement.
#[must_use]
pub fn statement_cancellation() -> Option<CancellationToken> {
    STATEMENT_CANCELLATION.with_borrow(Clone::clone)
}

/// Report the running statement's cancellation with the reason its token was canceled for, or `Ok` while it may continue. Work outside any statement has no cancellation to observe and continues.
pub fn check_statement_cancellation() -> Result<(), QueryCancelled> {
    STATEMENT_CANCELLATION
        .with_borrow(|token| token.as_ref().map_or(Ok(()), CancellationToken::check))
}

/// Runs the current thread under one statement's cancellation until it drops, restoring the enclosing statement's so nested statements and workers return to their caller's.
pub struct StatementCancellationScope {
    previous: Option<CancellationToken>,
    thread: PhantomData<Rc<()>>,
}

impl StatementCancellationScope {
    /// Enter `token` as the running statement's cancellation. Entering `None` runs the thread outside any statement, as a worker does for work dispatched from outside one, so it never observes the cancellation of a statement whose work it ran before.
    #[must_use]
    pub fn enter(token: impl Into<Option<CancellationToken>>) -> Self {
        Self {
            previous: STATEMENT_CANCELLATION.replace(token.into()),
            thread: PhantomData,
        }
    }
}

impl Drop for StatementCancellationScope {
    fn drop(&mut self) {
        STATEMENT_CANCELLATION.set(self.previous.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::CancellationReason;

    #[test]
    fn work_outside_a_statement_continues_without_a_token() {
        assert!(statement_cancellation().is_none());
        assert_eq!(check_statement_cancellation(), Ok(()));
    }

    #[test]
    fn a_statement_observes_its_token_with_the_reason_it_was_canceled_for() {
        let token = CancellationToken::new();
        let _statement = StatementCancellationScope::enter(token.clone());
        assert!(statement_cancellation().is_some_and(|current| current.shares_signal(&token)));
        assert_eq!(check_statement_cancellation(), Ok(()));
        token.cancel_with(CancellationReason::StatementTimeout);
        let cancelled = check_statement_cancellation().unwrap_err();
        assert_eq!(
            cancelled,
            QueryCancelled::new(CancellationReason::StatementTimeout)
        );
        assert_eq!(cancelled.sqlstate(), "57014");
        assert_eq!(
            cancelled.to_string(),
            "canceling statement due to statement timeout"
        );
        token.reset();
        assert_eq!(check_statement_cancellation(), Ok(()));
    }

    #[test]
    fn nested_statements_restore_the_enclosing_cancellation_on_every_exit() {
        let outer = CancellationToken::new();
        let inner = CancellationToken::new();
        inner.cancel();
        let _outer = StatementCancellationScope::enter(outer.clone());
        {
            let _inner = StatementCancellationScope::enter(inner.clone());
            assert!(statement_cancellation().is_some_and(|current| current.shares_signal(&inner)));
            assert_eq!(
                check_statement_cancellation(),
                Err(QueryCancelled::USER_REQUEST)
            );
            {
                let _detached = StatementCancellationScope::enter(None);
                assert!(statement_cancellation().is_none());
                assert_eq!(check_statement_cancellation(), Ok(()));
            }
            assert!(statement_cancellation().is_some_and(|current| current.shares_signal(&inner)));
        }
        assert!(statement_cancellation().is_some_and(|current| current.shares_signal(&outer)));
        assert_eq!(check_statement_cancellation(), Ok(()));
        let failed = std::panic::catch_unwind(|| {
            let _failing = StatementCancellationScope::enter(inner.clone());
            panic!("the statement fails inside its scope");
        });
        assert!(failed.is_err());
        assert!(statement_cancellation().is_some_and(|current| current.shares_signal(&outer)));
    }

    #[test]
    fn another_thread_runs_outside_the_statement_until_it_enters_the_token() {
        let token = CancellationToken::new();
        token.cancel();
        let _statement = StatementCancellationScope::enter(token.clone());
        let captured = statement_cancellation();
        std::thread::spawn(move || {
            assert_eq!(check_statement_cancellation(), Ok(()));
            let _worker = StatementCancellationScope::enter(captured);
            assert_eq!(
                check_statement_cancellation(),
                Err(QueryCancelled::USER_REQUEST)
            );
        })
        .join()
        .unwrap();
    }
}
