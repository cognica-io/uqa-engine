//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The notices a session queues for its client.

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use uqa_sql::semantics::parameters::catalog::message_levels;
use uqa_sql::SQLNotice;

/// The notices a session queues for its client, in the order they arrive. A notice below the session's `client_min_messages` is dropped as it arrives, as `PostgreSQL` decides whether a report reaches the client when it raises it (`errstart`), so a level that a function sets for its own body silences only the notices raised there.
pub struct NoticeQueue {
    notices: Mutex<VecDeque<SQLNotice>>,
    /// The session's `client_min_messages`, as a message level, which the session keeps current.
    client_level: Arc<AtomicU8>,
}

impl Default for NoticeQueue {
    fn default() -> Self {
        Self::new(Arc::new(AtomicU8::new(message_levels::NOTICE)))
    }
}

impl NoticeQueue {
    /// A queue that reads the session's `client_min_messages` from `client_level`.
    pub fn new(client_level: Arc<AtomicU8>) -> Self {
        Self {
            notices: Mutex::new(VecDeque::new()),
            client_level,
        }
    }

    /// Queue `notice` after the notices queued before it, unless its level does not reach the client.
    pub fn push(&self, notice: SQLNotice) {
        if notice
            .level
            .reaches_client(self.client_level.load(Ordering::Acquire))
        {
            self.notices.lock().push_back(notice);
        }
    }

    /// Take every queued notice in the order they arrived.
    pub fn take(&self) -> Vec<SQLNotice> {
        let notices = std::mem::take(&mut *self.notices.lock());
        notices.into()
    }

    /// Admit one transfer before removing the oldest notice. Rejection or unwinding leaves the
    /// queue unchanged. `prepare` runs under the queue lock and must not reenter the queue or
    /// execute statements that can emit notices. Success moves the original notice without
    /// copying its fields or shifting the remaining queue.
    pub fn try_take_with<T, E>(
        &self,
        prepare: impl FnOnce(&SQLNotice) -> Result<T, E>,
    ) -> Result<Option<(SQLNotice, T)>, E> {
        let mut notices = self.notices.lock();
        let Some(notice) = notices.front() else {
            return Ok(None);
        };
        let admitted = prepare(notice)?;
        Ok(Some((
            notices.pop_front().expect("admitted notice remains queued"),
            admitted,
        )))
    }

    /// The queued notices, left queued.
    pub fn snapshot(&self) -> Vec<SQLNotice> {
        self.notices.lock().iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.notices.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.notices.lock().is_empty()
    }
}

#[cfg(test)]
mod tests;
