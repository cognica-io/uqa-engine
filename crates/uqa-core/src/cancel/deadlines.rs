//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deadlines served by one timer thread for the process, as `PostgreSQL`'s timeout handler serves `statement_timeout` and the session timeouts: a deadline cancels a token when it passes, and a scheduled action runs when its time comes.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use super::{CancellationReason, TokenState};

/// What happens when a deadline passes.
enum Pending {
    /// Cancel a token that still exists.
    Cancel(Weak<TokenState>, CancellationReason),
    /// Run an action.
    Run(Box<dyn FnOnce() + Send>),
}

impl Pending {
    fn fire(self) {
        match self {
            Self::Cancel(token, reason) => {
                if let Some(token) = token.upgrade() {
                    token.cancel(reason);
                }
            }
            Self::Run(action) => action(),
        }
    }
}

/// The pending deadlines in the order they pass.
#[derive(Default)]
struct DeadlineQueue {
    next_id: u64,
    pending: BTreeMap<(Instant, u64), Pending>,
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
    fn lock(&self) -> MutexGuard<'_, DeadlineQueue> {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Fire each deadline once it passes, outside the queue's lock, so that what fires may arm or disarm other deadlines.
    fn run(&self) {
        let mut queue = self.lock();
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
            let pending = queue.pending.remove(&(at, id)).expect("first deadline");
            drop(queue);
            pending.fire();
            queue = self.lock();
        }
    }

    fn insert(&self, after: Duration, pending: Pending) -> (Instant, u64) {
        let at = Instant::now()
            .checked_add(after)
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(u64::from(u32::MAX)));
        let mut queue = self.lock();
        let id = queue.next_id;
        queue.next_id += 1;
        queue.pending.insert((at, id), pending);
        drop(queue);
        self.changed.notify_all();
        (at, id)
    }

    /// Disarm a deadline; `false` when it has already fired.
    fn remove(&self, key: (Instant, u64)) -> bool {
        self.lock().pending.remove(&key).is_some()
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
        Self {
            key: service().insert(after, Pending::Cancel(Arc::downgrade(token), reason)),
            token: Arc::clone(token),
            reason,
        }
    }
}

impl Drop for CancellationDeadline {
    fn drop(&mut self) {
        if !service().remove(self.key) {
            self.token.clear(self.reason);
        }
    }
}

/// An action that runs on the timer thread once its time comes, unless it is dropped first. The action must return promptly; long work belongs on a thread of its own.
#[must_use = "a scheduled action is canceled when it is dropped"]
pub struct ScheduledAction {
    key: (Instant, u64),
}

impl Drop for ScheduledAction {
    fn drop(&mut self) {
        service().remove(self.key);
    }
}

/// Run `action` once `after` has passed, unless the returned handle is dropped first.
pub fn schedule(after: Duration, action: impl FnOnce() + Send + 'static) -> ScheduledAction {
    ScheduledAction {
        key: service().insert(after, Pending::Run(Box::new(action))),
    }
}
