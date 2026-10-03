//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deadlines that cancel a token when they pass, served by one timer thread for the process, as `PostgreSQL`'s timeout handler serves `statement_timeout`.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use super::{CancellationReason, TokenState};

/// The pending deadlines in the order they pass.
#[derive(Default)]
struct DeadlineQueue {
    next_id: u64,
    pending: BTreeMap<(Instant, u64), (Weak<TokenState>, CancellationReason)>,
}

struct DeadlineService {
    queue: Mutex<DeadlineQueue>,
    changed: Condvar,
}

fn service() -> &'static DeadlineService {
    static SERVICE: OnceLock<DeadlineService> = OnceLock::new();
    SERVICE.get_or_init(|| {
        std::thread::Builder::new()
            .name("uqa-deadlines".into())
            .spawn(|| service().run())
            .expect("start the deadline timer thread");
        DeadlineService {
            queue: Mutex::new(DeadlineQueue::default()),
            changed: Condvar::new(),
        }
    })
}

impl DeadlineService {
    fn run(&self) {
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            let now = Instant::now();
            let Some((&(at, id), _)) = queue.pending.first_key_value() else {
                queue = self
                    .changed
                    .wait(queue)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                continue;
            };
            if at > now {
                queue = self
                    .changed
                    .wait_timeout(queue, at - now)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
                continue;
            }
            let (token, reason) = queue.pending.remove(&(at, id)).expect("first deadline");
            if let Some(token) = token.upgrade() {
                token.cancel(reason);
            }
        }
    }
}

/// A deadline that cancels its token with its reason when it passes; dropping it before then disarms it, and dropping it after clears a cancellation of its reason that nothing has observed, so that it cannot cancel what runs next.
#[must_use = "a deadline is disarmed when it is dropped"]
pub struct CancellationDeadline {
    key: (Instant, u64),
    token: Arc<TokenState>,
    reason: CancellationReason,
}

impl CancellationDeadline {
    pub(super) fn arm(
        token: &Arc<TokenState>,
        after: Duration,
        reason: CancellationReason,
    ) -> Self {
        let service = service();
        let at = Instant::now() + after;
        let mut queue = service
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = queue.next_id;
        queue.next_id += 1;
        queue
            .pending
            .insert((at, id), (Arc::downgrade(token), reason));
        drop(queue);
        service.changed.notify_all();
        Self {
            key: (at, id),
            token: Arc::clone(token),
            reason,
        }
    }
}

impl Drop for CancellationDeadline {
    fn drop(&mut self) {
        let service = service();
        let removed = service
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .remove(&self.key)
            .is_some();
        if !removed {
            self.token.clear(self.reason);
        }
    }
}
