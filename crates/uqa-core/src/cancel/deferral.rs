//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Thread-scoped cancellation deferral for resource finalization, without consuming a signal.

use super::CancellationToken;
use std::sync::{Mutex, MutexGuard};
use std::thread::ThreadId;

#[derive(Debug, Default)]
pub(super) struct CancellationDeferrals {
    threads: Mutex<Vec<ThreadId>>,
}

impl CancellationDeferrals {
    fn threads(&self) -> MutexGuard<'_, Vec<ThreadId>> {
        self.threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn active(&self) -> bool {
        self.threads().contains(&std::thread::current().id())
    }
}

struct Deferral<'a> {
    owner: &'a CancellationDeferrals,
    thread: ThreadId,
}

impl Drop for Deferral<'_> {
    fn drop(&mut self) {
        let mut threads = self.owner.threads();
        let position = threads
            .iter()
            .rposition(|thread| *thread == self.thread)
            .expect("active cancellation deferral");
        threads.remove(position);
    }
}

impl CancellationToken {
    /// Finish cleanup on this thread even after cancellation or permanent termination. Clones of this token defer their cancellation checks in the callback; other tokens and threads remain cancellable. Signals and lock-timeout settings are never cleared, and checks resume after return or unwind.
    pub fn with_deferred_cancellation<R>(&self, cleanup: impl FnOnce() -> R) -> R {
        let thread = std::thread::current().id();
        self.state.deferrals.threads().push(thread);
        let _deferral = Deferral {
            owner: &self.state.deferrals,
            thread,
        };
        cleanup()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancel::{CancellationReason, QueryCancelled};
    use std::time::{Duration, Instant};

    #[test]
    fn deferral_retains_the_signal_and_distinguishes_tokens_and_threads() {
        let token = CancellationToken::new();
        let clone = token.clone();
        let other = CancellationToken::new();
        token.cancel();
        other.cancel();
        token.with_deferred_cancellation(|| {
            assert!(clone.check().is_ok());
            assert_eq!(token.reason(), Some(CancellationReason::UserRequest));
            assert_eq!(other.check(), Err(QueryCancelled::USER_REQUEST));
            assert_eq!(
                std::thread::spawn(move || clone.check()).join().unwrap(),
                Err(QueryCancelled::USER_REQUEST)
            );
            other.with_deferred_cancellation(|| {
                assert!(token.check().is_ok());
                assert!(other.check().is_ok());
            });
            assert!(token.check().is_ok());
            assert_eq!(other.check(), Err(QueryCancelled::USER_REQUEST));
        });
        assert_eq!(token.check(), Err(QueryCancelled::USER_REQUEST));
    }

    #[test]
    fn late_termination_survives_nested_deferral_and_unwind() {
        let token = CancellationToken::new();
        let result = std::panic::catch_unwind(|| {
            token.with_deferred_cancellation(|| {
                token.with_deferred_cancellation(|| {
                    token.cancel_with(CancellationReason::IdleSessionTimeout);
                    assert!(token.check().is_ok());
                });
                assert!(token.check().is_ok());
                panic!("cleanup unwind");
            });
        });
        assert!(result.is_err());
        token.reset();
        assert_eq!(
            token.check().unwrap_err().reason,
            CancellationReason::IdleSessionTimeout
        );
    }

    #[test]
    fn cleanup_waits_defer_expired_lock_deadlines_without_busy_sleeping() {
        let token = CancellationToken::new();
        token.set_lock_timeout(Some(Duration::from_millis(1)));
        let started = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        let slice = Duration::from_millis(5);
        token.cancel();
        token.with_deferred_cancellation(|| {
            assert!(token.check_lock_wait(started).is_ok());
            assert_eq!(token.lock_wait_slice(started, slice), slice);
            assert!(token.sleep(Duration::from_millis(1)).is_ok());
        });
        assert_eq!(token.check(), Err(QueryCancelled::USER_REQUEST));
        token.reset();
        assert_eq!(
            token.check_lock_wait(started).unwrap_err().reason,
            CancellationReason::LockTimeout
        );
    }
}
