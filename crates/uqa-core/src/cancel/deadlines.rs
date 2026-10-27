//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deadlines share a timer thread retained by their tokens and scheduled actions. The final owner stops and joins the worker; a later owner can start a new service.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{CancellationReason, TokenState};

/// What happens when a deadline passes.
enum Pending {
    /// Cancel a token that still exists.
    Cancel(Arc<PendingCancellation>),
    /// Run an action.
    Run(Box<dyn FnOnce() + Send>),
}

impl Pending {
    fn fire(self) {
        match self {
            Self::Cancel(cancellation) => cancellation.fire(),
            Self::Run(action) => action(),
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CancellationPhase {
    Armed,
    Fired,
    Disarmed,
}

struct PendingCancellation {
    token: Weak<TokenState>,
    reason: CancellationReason,
    phase: Mutex<CancellationPhase>,
}

impl PendingCancellation {
    fn lock(&self) -> MutexGuard<'_, CancellationPhase> {
        self.phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn fire(&self) {
        let mut phase = self.lock();
        if *phase == CancellationPhase::Armed {
            if let Some(token) = self.token.upgrade() {
                token.cancel(self.reason);
            }
            *phase = CancellationPhase::Fired;
        }
    }

    fn disarm(&self, token: &TokenState) {
        // Hold the same lock through cancellation and clearing: a dequeued timer must
        // either finish before this clear or observe Disarmed without touching the token.
        let mut phase = self.lock();
        if *phase == CancellationPhase::Fired {
            token.clear(self.reason);
        }
        *phase = CancellationPhase::Disarmed;
    }
}

/// The pending deadlines in the order they pass.
#[derive(Default)]
struct DeadlineQueue {
    next_id: u64,
    pending: BTreeMap<(Instant, u64), Pending>,
    stopped: bool,
}

struct DeadlineWorker {
    queue: Mutex<DeadlineQueue>,
    changed: Condvar,
}

/// The worker owns only its queue, so it cannot keep its callers alive.
pub(super) struct DeadlineService {
    state: Arc<DeadlineWorker>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for DeadlineService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeadlineService")
            .finish_non_exhaustive()
    }
}

pub(super) fn service() -> Arc<DeadlineService> {
    static SERVICE: Mutex<Weak<DeadlineService>> = Mutex::new(Weak::new());
    let mut current = SERVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(service) = current.upgrade() {
        return service;
    }
    let service = DeadlineService::start();
    *current = Arc::downgrade(&service);
    service
}

impl DeadlineService {
    fn start() -> Arc<Self> {
        let state = Arc::new(DeadlineWorker {
            queue: Mutex::new(DeadlineQueue::default()),
            changed: Condvar::new(),
        });
        let running = Arc::clone(&state);
        let worker = std::thread::Builder::new()
            .name("uqa-deadlines".into())
            .spawn(move || running.run())
            .expect("start the deadline timer thread");
        Arc::new(Self {
            state,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub(super) fn schedule(
        self: &Arc<Self>,
        after: Duration,
        action: impl FnOnce() + Send + 'static,
    ) -> ScheduledAction {
        ScheduledAction {
            key: self.state.insert(after, Pending::Run(Box::new(action))),
            service: Arc::clone(self),
        }
    }
}

impl Drop for DeadlineService {
    fn drop(&mut self) {
        self.state.lock().stopped = true;
        self.state.changed.notify_one();
        if let Some(worker) = self
            .worker
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            // An action may release the last owner on this worker. It returns
            // to the stopped queue itself, without trying to join its own thread.
            if worker.thread().id() != std::thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}

impl DeadlineWorker {
    fn lock(&self) -> MutexGuard<'_, DeadlineQueue> {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Fire each deadline once it passes, outside the queue's lock, so that what fires may arm or disarm other deadlines.
    fn run(&self) {
        let mut queue = self.lock();
        loop {
            if queue.stopped {
                return;
            }
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

    /// Remove a queued deadline; `false` when the timer has already dequeued it.
    fn remove(&self, key: (Instant, u64)) -> bool {
        let pending = self.lock().pending.remove(&key);
        self.changed.notify_one();
        // Captured values may own other scheduled actions. Drop them only
        // after releasing the queue lock, because their cleanup removes them.
        pending.is_some()
    }
}

/// A deadline that cancels its token with its reason when it passes; dropping it before then disarms it, and dropping it after clears a cancellation of its reason that nothing has observed, so that it cannot cancel what runs next.
#[must_use = "a deadline is disarmed when it is dropped"]
pub struct CancellationDeadline {
    key: (Instant, u64),
    service: Arc<DeadlineService>,
    token: Arc<TokenState>,
    cancellation: Arc<PendingCancellation>,
}

impl CancellationDeadline {
    pub(super) fn arm(
        token: &Arc<TokenState>,
        after: Duration,
        reason: CancellationReason,
    ) -> Self {
        let cancellation = Arc::new(PendingCancellation {
            token: Arc::downgrade(token),
            reason,
            phase: Mutex::new(CancellationPhase::Armed),
        });
        let service = Arc::clone(token.deadlines.get_or_init(service));
        Self {
            key: service
                .state
                .insert(after, Pending::Cancel(Arc::clone(&cancellation))),
            service,
            token: Arc::clone(token),
            cancellation,
        }
    }
}

impl Drop for CancellationDeadline {
    fn drop(&mut self) {
        self.cancellation.disarm(&self.token);
        self.service.state.remove(self.key);
    }
}

/// An action that runs on the timer thread once its time comes, unless it is dropped first. The action must return promptly; long work belongs on a thread of its own.
#[must_use = "a scheduled action is canceled when it is dropped"]
pub struct ScheduledAction {
    key: (Instant, u64),
    service: Arc<DeadlineService>,
}

impl Drop for ScheduledAction {
    fn drop(&mut self) {
        self.service.state.remove(self.key);
    }
}

/// Run `action` once `after` has passed, unless the returned handle is dropped first.
pub fn schedule(after: Duration, action: impl FnOnce() + Send + 'static) -> ScheduledAction {
    service().schedule(after, action)
}

#[cfg(test)]
mod tests;
