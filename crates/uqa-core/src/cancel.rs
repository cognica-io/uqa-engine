//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query cancellation support.
//!
//! A [`CancellationToken`] is a cheap-to-clone, thread-safe signal
//! stored on `Engine` and propagated into every
//! `PhysicalOperator` / `Operator` hot loop. Operators call
//! [`CancellationToken::check`] at chunk boundaries; once the token
//! has been canceled from another thread, by a deadline or by a client,
//! `check` returns [`QueryCancelled`] with the [`CancellationReason`],
//! which surfaces to the SQL layer with the message and SQLSTATE
//! `PostgreSQL` reports for that reason.
//!
//! ```rust
//! use uqa_core::cancel::{CancellationReason, CancellationToken, QueryCancelled};
//!
//! let tok = CancellationToken::new();
//! let probe = tok.clone();
//! tok.cancel();
//! assert!(probe.is_cancelled());
//! assert_eq!(probe.check(), Err(QueryCancelled::new(CancellationReason::UserRequest)));
//! ```
//!
//! The token is a `Clone`-by-`Arc` handle: every clone speaks to the
//! same underlying signal, so issuing `engine.cancel()` from one thread
//! is immediately visible to any operator that received a clone of
//! the token before the cancellation.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

mod cancelled;
mod deadlines;

pub use cancelled::{CancellationReason, QueryCancelled};
pub use deadlines::CancellationDeadline;

/// `PostgreSQL` SQLSTATE `57014` (`query_canceled`).
pub const SQLSTATE_QUERY_CANCELED: &str = "57014";

/// The signal every clone of a token shares.
#[derive(Debug, Default)]
struct TokenState {
    /// 0 while not canceled, otherwise the code of the reason.
    reason: AtomicU8,
    /// The session's `lock_timeout` in milliseconds; 0 lets a lock wait last until the lock is granted.
    lock_timeout_ms: AtomicU64,
    sleepers: Mutex<()>,
    wake: Condvar,
}

impl TokenState {
    /// Cancel with `reason` unless already canceled, and wake every sleeper.
    fn cancel(&self, reason: CancellationReason) {
        let _ = self
            .reason
            .compare_exchange(0, reason.code(), Ordering::AcqRel, Ordering::Acquire);
        let _sleepers = self
            .sleepers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.wake.notify_all();
    }

    /// Clear a cancellation of `reason`, leaving any other.
    fn clear(&self, reason: CancellationReason) {
        let _ = self
            .reason
            .compare_exchange(reason.code(), 0, Ordering::AcqRel, Ordering::Acquire);
    }

    fn check(&self) -> Result<(), QueryCancelled> {
        match self.reason.load(Ordering::Acquire) {
            0 => Ok(()),
            code => Err(QueryCancelled::new(CancellationReason::from_code(code))),
        }
    }
}

/// Thread-safe cancellation token for query execution.
///
/// Cloning is `O(1)` and every clone observes the same signal. Once
/// the token has been canceled, every subsequent [`Self::check`] returns
/// [`QueryCancelled`] with the reason until [`Self::reset`] is called or
/// a deadline of that reason is dropped after it passed.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    state: Arc<TokenState>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Signal cancellation at a client's request. Subsequent [`Self::check`] / [`Self::is_cancelled`]
    /// observe the signal across all clones of this token.
    pub fn cancel(&self) {
        self.cancel_with(CancellationReason::UserRequest);
    }

    /// Signal cancellation for `reason`, unless the token is already canceled.
    pub fn cancel_with(&self, reason: CancellationReason) {
        self.state.cancel(reason);
    }

    /// Clear the cancellation signal for the next query. Operators
    /// holding a clone of this token through their lifetime see the
    /// reset on the next `check`.
    pub fn reset(&self) {
        self.state.reason.store(0, Ordering::Release);
    }

    /// Clear a cancellation of `reason`, leaving any other in place, as a `PL/pgSQL` handler that catches `query_canceled` consumes the cancellation it caught.
    pub fn clear(&self, reason: CancellationReason) {
        self.state.clear(reason);
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.reason.load(Ordering::Acquire) != 0
    }

    /// The reason the token was canceled for, if it was.
    pub fn reason(&self) -> Option<CancellationReason> {
        self.state.check().err().map(|cancelled| cancelled.reason)
    }

    /// Whether both handles observe the same signal, independently of its current cancelled state.
    pub fn shares_signal(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    /// Return [`QueryCancelled`] if cancellation was signalled.
    /// `Ok(())` otherwise.
    ///
    /// Designed for the inner loop of every operator: a single
    /// atomic load on the happy path.
    pub fn check(&self) -> Result<(), QueryCancelled> {
        self.state.check()
    }

    /// Cancel the token with `reason` once `after` has passed, unless the returned deadline is dropped first.
    pub fn deadline(&self, after: Duration, reason: CancellationReason) -> CancellationDeadline {
        CancellationDeadline::arm(&self.state, after, reason)
    }

    /// Set the `lock_timeout` that lock waits under this token honor; `None` lets a wait last until the lock is granted.
    pub fn set_lock_timeout(&self, timeout: Option<Duration>) {
        let millis = timeout.map_or(0, |timeout| {
            u64::try_from(timeout.as_millis())
                .unwrap_or(u64::MAX)
                .max(1)
        });
        self.state.lock_timeout_ms.store(millis, Ordering::Release);
    }

    /// The `lock_timeout` lock waits under this token honor.
    pub fn lock_timeout(&self) -> Option<Duration> {
        match self.state.lock_timeout_ms.load(Ordering::Acquire) {
            0 => None,
            millis => Some(Duration::from_millis(millis)),
        }
    }

    /// Check a lock wait that began at `started`: a canceled token, or a wait that has outlasted `lock_timeout`, which reports `canceling statement due to lock timeout` (`55P03`).
    pub fn check_lock_wait(&self, started: Instant) -> Result<(), QueryCancelled> {
        self.check()?;
        if self
            .lock_timeout()
            .is_some_and(|timeout| started.elapsed() >= timeout)
        {
            return Err(QueryCancelled::new(CancellationReason::LockTimeout));
        }
        Ok(())
    }

    /// How long a lock wait that began at `started` may sleep before it checks the lock again: at most `slice`, and no longer than the rest of `lock_timeout`.
    pub fn lock_wait_slice(&self, started: Instant, slice: Duration) -> Duration {
        self.lock_timeout().map_or(slice, |timeout| {
            slice.min(timeout.saturating_sub(started.elapsed()))
        })
    }

    /// Sleep for `duration` unless the token is canceled first, which ends the sleep at once.
    pub fn sleep(&self, duration: Duration) -> Result<(), QueryCancelled> {
        let until = Instant::now().checked_add(duration);
        let mut sleepers = self
            .state
            .sleepers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            self.check()?;
            let remaining = match until {
                Some(until) => until.saturating_duration_since(Instant::now()),
                None => Duration::from_secs(600),
            };
            if remaining.is_zero() {
                return Ok(());
            }
            sleepers = self
                .state
                .wake
                .wait_timeout(sleepers, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
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
        assert_eq!(observer.check(), Err(QueryCancelled::USER_REQUEST));
    }

    #[test]
    fn signal_identity_distinguishes_independent_tokens_with_equal_states() {
        let first = CancellationToken::new();
        let retained = first.clone();
        let independent = CancellationToken::new();
        assert!(first.shares_signal(&retained));
        assert!(!first.shares_signal(&independent));
        first.cancel();
        independent.cancel();
        assert!(first.shares_signal(&retained));
        assert!(!first.shares_signal(&independent));
        first.reset();
        assert!(first.shares_signal(&retained));
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
        assert_eq!(res, Err(QueryCancelled::USER_REQUEST));
    }

    #[test]
    fn a_token_keeps_the_first_reason_it_was_canceled_for() {
        let token = CancellationToken::new();
        token.cancel_with(CancellationReason::StatementTimeout);
        token.cancel();
        assert_eq!(
            token.check(),
            Err(QueryCancelled::new(CancellationReason::StatementTimeout))
        );
        assert_eq!(token.reason(), Some(CancellationReason::StatementTimeout));
        assert_eq!(
            token.check().unwrap_err().to_string(),
            "canceling statement due to statement timeout"
        );
        token.clear(CancellationReason::UserRequest);
        assert!(token.is_cancelled());
        token.clear(CancellationReason::StatementTimeout);
        assert!(!token.is_cancelled());
        assert_eq!(QueryCancelled::USER_REQUEST.sqlstate(), "57014");
        assert_eq!(
            QueryCancelled::new(CancellationReason::LockTimeout).sqlstate(),
            "55P03"
        );
    }

    #[test]
    fn a_deadline_cancels_once_it_passes_and_clears_what_it_left_when_dropped() {
        let token = CancellationToken::new();
        let deadline = token.deadline(
            std::time::Duration::from_millis(20),
            CancellationReason::StatementTimeout,
        );
        let started = std::time::Instant::now();
        assert_eq!(
            token.sleep(std::time::Duration::from_secs(10)),
            Err(QueryCancelled::new(CancellationReason::StatementTimeout))
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        drop(deadline);
        assert!(!token.is_cancelled());
        let early = token.deadline(
            std::time::Duration::from_secs(60),
            CancellationReason::StatementTimeout,
        );
        drop(early);
        assert!(token.sleep(std::time::Duration::from_millis(5)).is_ok());
        token.cancel();
        let fired = token.deadline(
            std::time::Duration::from_millis(1),
            CancellationReason::StatementTimeout,
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        drop(fired);
        assert_eq!(token.reason(), Some(CancellationReason::UserRequest));
    }

    #[test]
    fn lock_waits_end_once_they_outlast_the_lock_timeout() {
        let token = CancellationToken::new();
        let started = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_millis(30))
            .unwrap();
        assert!(token.check_lock_wait(started).is_ok());
        let slice = std::time::Duration::from_millis(50);
        assert_eq!(token.lock_wait_slice(started, slice), slice);
        token.set_lock_timeout(Some(std::time::Duration::from_millis(20)));
        assert_eq!(
            token.check_lock_wait(started),
            Err(QueryCancelled::new(CancellationReason::LockTimeout))
        );
        assert_eq!(
            token.lock_wait_slice(started, slice),
            std::time::Duration::ZERO
        );
        assert!(!token.is_cancelled());
        token.set_lock_timeout(None);
        assert_eq!(token.lock_timeout(), None);
    }

    #[test]
    fn sleep_ends_at_once_when_another_thread_cancels() {
        let token = CancellationToken::new();
        let canceler = token.clone();
        let started = std::time::Instant::now();
        let handle = thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(20));
            canceler.cancel();
        });
        assert_eq!(
            token.sleep(std::time::Duration::from_secs(30)),
            Err(QueryCancelled::USER_REQUEST)
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        handle.join().unwrap();
    }
}
