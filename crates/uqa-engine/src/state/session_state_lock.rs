//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The lock over a session's transactional values, which keeps the session's notice threshold current through every change, rollback and restore of those values.

use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use uqa_sql::semantics::parameters::catalog::{find_parameter, message_levels};
use uqa_sql::semantics::parameters::definition::ParameterKind;

use crate::SessionStateSnapshot;

/// The session's transactional values behind one lock. Each write, whatever restores or changes the values, leaves `client_level` at the `client_min_messages` the values hold, which the session's notice queue reads as notices arrive.
pub(crate) struct SessionStateLock {
    state: RwLock<SessionStateSnapshot>,
    client_level: Arc<AtomicU8>,
    /// The level when no assignment in the values names `client_min_messages`: the client's startup value or the default.
    reset_client_level: AtomicU8,
}

impl SessionStateLock {
    pub(crate) fn new(state: SessionStateSnapshot) -> Self {
        Self {
            state: RwLock::new(state),
            client_level: Arc::new(AtomicU8::new(message_levels::NOTICE)),
            reset_client_level: AtomicU8::new(message_levels::NOTICE),
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
}

/// Exclusive access to the session's values; dropping it publishes the `client_min_messages` they hold.
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
        let level = self
            .guard
            .session_vars
            .get("client_min_messages")
            .and_then(|setting| message_level(setting))
            .unwrap_or_else(|| self.lock.reset_client_level.load(Ordering::Acquire));
        self.lock.client_level.store(level, Ordering::Release);
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
