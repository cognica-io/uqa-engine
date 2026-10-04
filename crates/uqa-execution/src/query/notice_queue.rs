//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The notices a session queues for its client.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use uqa_sql::semantics::parameters::catalog::message_levels;
use uqa_sql::SQLNotice;

/// The notices a session queues for its client, in the order they arrive. A notice below the session's `client_min_messages` is dropped as it arrives, as `PostgreSQL` decides whether a report reaches the client when it raises it (`errstart`), so a level that a function sets for its own body silences only the notices raised there.
pub struct NoticeQueue {
    notices: Mutex<Vec<SQLNotice>>,
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
            notices: Mutex::new(Vec::new()),
            client_level,
        }
    }

    /// Queue `notice` after the notices queued before it, unless its level does not reach the client.
    pub fn push(&self, notice: SQLNotice) {
        if notice
            .level
            .reaches_client(self.client_level.load(Ordering::Acquire))
        {
            self.notices.lock().push(notice);
        }
    }

    /// Take every queued notice in the order they arrived.
    pub fn take(&self) -> Vec<SQLNotice> {
        std::mem::take(&mut *self.notices.lock())
    }

    /// The queued notices, left queued.
    pub fn snapshot(&self) -> Vec<SQLNotice> {
        self.notices.lock().clone()
    }

    pub fn len(&self) -> usize {
        self.notices.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.notices.lock().is_empty()
    }
}
