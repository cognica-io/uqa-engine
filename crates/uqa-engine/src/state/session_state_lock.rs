//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The lock over transactional session values publishes settings needed outside the lock through every change, rollback and restore.

use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use uqa_sql::semantics::parameters::catalog::{find_parameter, message_levels};
use uqa_sql::semantics::parameters::definition::ParameterKind;

use crate::SessionStateSnapshot;

/// The session's transactional values behind one lock. Each write, whatever restores or changes the values, leaves `client_level` at the `client_min_messages` the values hold, which the session's notice queue reads as notices arrive, and the session's cancellation token at the `lock_timeout` they hold, which lock waits read as they wait.
pub(crate) struct SessionStateLock {
    state: RwLock<SessionStateSnapshot>,
    client_level: Arc<AtomicU8>,
    /// The level when no assignment in the values names `client_min_messages`: the client's startup value or the default.
    reset_client_level: AtomicU8,
    /// The session's cancellation token, whose lock waits honor `lock_timeout`.
    cancellation: OnceLock<uqa_core::CancellationToken>,
    /// The `lock_timeout` in milliseconds when no assignment in the values names it: the client's startup value or the default.
    reset_lock_timeout_ms: AtomicU64,
    /// Physical I/O can run while session state is locked. Its clock reads the effective setting published with every completed state write.
    io_timing: Arc<AtomicBool>,
    reset_io_timing: AtomicBool,
}

impl SessionStateLock {
    pub(crate) fn new(state: SessionStateSnapshot) -> Self {
        let io_timing = state
            .session_vars
            .get("track_io_timing")
            .is_some_and(|value| value == "on");
        Self {
            state: RwLock::new(state),
            client_level: Arc::new(AtomicU8::new(message_levels::NOTICE)),
            reset_client_level: AtomicU8::new(message_levels::NOTICE),
            cancellation: OnceLock::new(),
            reset_lock_timeout_ms: AtomicU64::new(0),
            io_timing: Arc::new(AtomicBool::new(io_timing)),
            reset_io_timing: AtomicBool::new(false),
        }
    }

    /// Keep the `lock_timeout` of the session's values on `token`, the session's cancellation token.
    pub(crate) fn attach_cancellation(&self, token: &uqa_core::CancellationToken) {
        if self.cancellation.set(token.clone()).is_ok() {
            drop(self.write());
        }
    }

    pub(crate) fn read(&self) -> RwLockReadGuard<'_, SessionStateSnapshot> {
        self.state.read()
    }

    pub(crate) fn try_read_for(
        &self,
        timeout: std::time::Duration,
    ) -> Option<RwLockReadGuard<'_, SessionStateSnapshot>> {
        self.state.try_read_for(timeout)
    }

    #[cfg(test)]
    pub(crate) fn try_write(&self) -> Option<SessionStateWriteGuard<'_>> {
        self.state
            .try_write()
            .map(|guard| SessionStateWriteGuard { guard, lock: self })
    }

    #[cfg(test)]
    pub(crate) fn is_locked(&self) -> bool {
        self.state.is_locked()
    }

    pub(crate) fn write(&self) -> SessionStateWriteGuard<'_> {
        SessionStateWriteGuard {
            guard: self.state.write(),
            lock: self,
        }
    }

    /// The level that the session's notice queue reads.
    pub(crate) fn client_level(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.client_level)
    }

    /// Set the level of a session whose values do not assign `client_min_messages`, as the client's startup value does.
    pub(crate) fn set_reset_client_level(&self, level: u8) {
        self.reset_client_level.store(level, Ordering::Release);
        drop(self.write());
    }

    /// Set the `lock_timeout` of a session whose values do not assign it, as the client's startup value does.
    pub(crate) fn set_reset_lock_timeout(&self, milliseconds: u64) {
        self.reset_lock_timeout_ms
            .store(milliseconds, Ordering::Release);
        drop(self.write());
    }

    pub(crate) fn io_timing(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.io_timing)
    }

    pub(crate) fn set_reset_io_timing(&self, enabled: bool) {
        self.reset_io_timing.store(enabled, Ordering::Release);
        drop(self.write());
    }
}

/// Exclusive access to the session's values; dropping it publishes the effective settings to consumers outside the lock.
pub(crate) struct SessionStateWriteGuard<'a> {
    guard: RwLockWriteGuard<'a, SessionStateSnapshot>,
    lock: &'a SessionStateLock,
}

impl Deref for SessionStateWriteGuard<'_> {
    type Target = SessionStateSnapshot;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl DerefMut for SessionStateWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl Drop for SessionStateWriteGuard<'_> {
    fn drop(&mut self) {
        let io_timing = self.guard.session_vars.get("track_io_timing").map_or_else(
            || self.lock.reset_io_timing.load(Ordering::Acquire),
            |setting| setting == "on",
        );
        self.lock.io_timing.store(io_timing, Ordering::Release);
        let level = self
            .guard
            .session_vars
            .get("client_min_messages")
            .and_then(|setting| message_level(setting))
            .unwrap_or_else(|| self.lock.reset_client_level.load(Ordering::Acquire));
        self.lock.client_level.store(level, Ordering::Release);
        if let Some(token) = self.lock.cancellation.get() {
            let milliseconds = self
                .guard
                .session_vars
                .get("lock_timeout")
                .and_then(|setting| setting.parse::<u64>().ok())
                .unwrap_or_else(|| self.lock.reset_lock_timeout_ms.load(Ordering::Acquire));
            token
                .set_lock_timeout((milliseconds != 0).then(|| Duration::from_millis(milliseconds)));
        }
    }
}

/// The message level of a `client_min_messages` setting.
pub(crate) fn message_level(setting: &str) -> Option<u8> {
    let ParameterKind::Enum { options, .. } = find_parameter("client_min_messages")?.kind else {
        return None;
    };
    options
        .iter()
        .find(|option| option.name == setting)
        .map(|option| option.value)
}
