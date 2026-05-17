//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query cancellation support.
//!
//! Mirrors UQA `cancel` from the canonical UQA behavior. A
//! [`CancellationToken`] is a cheap-to-clone, thread-safe one-shot
//! flag stored on `Engine` and propagated into every
//! `PhysicalOperator` / `Operator` hot loop. Operators call
//! [`CancellationToken::check`] at chunk boundaries; if the flag has
//! been set from another thread, `check` returns
//! [`QueryCancelled`] which surfaces to the SQL layer as
//! `PostgreSQL` `SQLSTATE 57014` (`query_canceled`).
//!
//! ```rust
//! use uqa_core::cancel::{CancellationToken, QueryCancelled};
//!
//! let tok = CancellationToken::new();
//! let probe = tok.clone();
//! tok.cancel();
//! assert!(probe.is_cancelled());
//! assert!(matches!(probe.check(), Err(QueryCancelled)));
//! ```
//!
//! The token is a `Clone`-by-`Arc` handle: every clone speaks to the
//! same underlying flag, so issuing `engine.cancel()` from one thread
//! is immediately visible to any operator that received a clone of
//! the token before the cancellation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use thiserror::Error;

/// Raised when a query is cancelled by user request. Matches
/// `PostgreSQL` `SQLSTATE 57014` (`query_canceled`); the `Display`
/// payload mirrors the canonical UQA behavior's exception message so logs
/// stay aligned across the two implementations.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
#[error("canceling statement due to user request")]
pub struct QueryCancelled;

/// `PostgreSQL` SQLSTATE for [`QueryCancelled`].
pub const SQLSTATE_QUERY_CANCELED: &str = "57014";

/// Thread-safe cancellation token for query execution.
///
/// Uses an [`AtomicBool`] behind an [`Arc`] so cloning is `O(1)` and
/// every clone observes the same cancellation flag. Once
/// [`CancellationToken::cancel`] has been called, every subsequent
/// [`CancellationToken::check`] returns [`QueryCancelled`] until
/// [`CancellationToken::reset`] is called.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Signal cancellation. Subsequent [`Self::check`] / [`Self::is_cancelled`]
    /// observe the flag as set across all clones of this token.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    /// Clear the cancellation signal for the next query. Operators
    /// holding a clone of this token through their lifetime see the
    /// reset on the next `check`.
    pub fn reset(&self) {
        self.flag.store(false, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Return [`QueryCancelled`] if cancellation was signalled.
    /// `Ok(())` otherwise.
    ///
    /// Designed for the inner loop of every operator: a single
    /// relaxed-ordered atomic load on the happy path.
    pub fn check(&self) -> Result<(), QueryCancelled> {
        if self.is_cancelled() {
            Err(QueryCancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn fresh_token_is_not_cancelled() {
        let tok = CancellationToken::new();
        assert!(!tok.is_cancelled());
        assert!(tok.check().is_ok());
    }

    #[test]
    fn cancel_propagates_through_clone() {
        let tok = CancellationToken::new();
        let observer = tok.clone();
        tok.cancel();
        assert!(observer.is_cancelled());
        assert_eq!(observer.check(), Err(QueryCancelled));
    }

    #[test]
    fn reset_clears_signal() {
        let tok = CancellationToken::new();
        tok.cancel();
        tok.reset();
        assert!(!tok.is_cancelled());
        assert!(tok.check().is_ok());
    }

    #[test]
    fn cancel_visible_across_threads() {
        let tok = CancellationToken::new();
        let worker = tok.clone();
        let handle = thread::spawn(move || {
            // Spin until the parent cancels (test-only; real
            // operators check at chunk boundaries instead).
            while !worker.is_cancelled() {
                std::hint::spin_loop();
            }
            worker.check()
        });
        tok.cancel();
        let res = handle.join().unwrap();
        assert_eq!(res, Err(QueryCancelled));
    }
}
