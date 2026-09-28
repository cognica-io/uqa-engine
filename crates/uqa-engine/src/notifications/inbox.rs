//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained subscription output with count and shared byte admission before copying.

mod asynchronous;
mod batch;
pub(super) use batch::PreparedNotifications;

use parking_lot::{Condvar, Mutex};
use std::{
    sync::Arc,
    task::Poll,
    time::{Duration, Instant},
};
use uqa_core::{
    memory::{BudgetedDeque, BudgetedString, MemoryBudget, MemoryError, MemoryReservation},
    notifications::{
        NotificationEvent, NotificationFailureKind, NotificationIdentity, SQLNotification,
    },
};

use super::subscription::{
    NotificationSubscriptionError, NotificationSubscriptionOptions, NotificationWait,
};

pub(super) struct SubscriptionInbox {
    pub(super) options: NotificationSubscriptionOptions,
    pub(super) memory: MemoryBudget,
    identity: NotificationIdentity,
    state: Mutex<InboxState>,
    wake: Condvar,
}

struct InboxState {
    queue: BudgetedDeque<BufferedNotification>,
    reserved: usize,
    next_sequence: Option<u64>,
    closed: bool,
    failure: Option<NotificationSubscriptionError>,
    async_waiting: bool,
    async_waker: Option<std::task::Waker>,
}

struct BufferedNotification {
    value: SQLNotification,
    _memory: MemoryReservation,
}

impl BufferedNotification {
    fn copy(
        process_id: i32,
        channel: &str,
        payload: &str,
        memory: &MemoryBudget,
    ) -> Result<Self, MemoryError> {
        let mut channel_copy = BudgetedString::new(memory);
        channel_copy.push_str(channel)?;
        let mut payload_copy = BudgetedString::new(memory);
        payload_copy.push_str(payload)?;
        let (channel, mut reservation) = channel_copy.into_parts();
        let (payload, payload_memory) = payload_copy.into_parts();
        reservation.absorb(payload_memory);
        Ok(Self {
            value: SQLNotification {
                process_id,
                channel,
                payload,
            },
            _memory: reservation,
        })
    }
}

impl SubscriptionInbox {
    pub(super) fn new(
        identity: NotificationIdentity,
        options: NotificationSubscriptionOptions,
    ) -> Arc<Self> {
        let memory = MemoryBudget::new(options.max_queued_bytes);
        Arc::new(Self {
            options,
            state: Mutex::new(InboxState {
                queue: BudgetedDeque::new(&memory),
                reserved: 0,
                next_sequence: Some(1),
                closed: false,
                failure: None,
                async_waiting: false,
                async_waker: None,
            }),
            memory,
            identity,
            wake: Condvar::new(),
        })
    }

    pub(super) fn identity(&self) -> &NotificationIdentity {
        &self.identity
    }

    pub(super) fn is_closed(&self) -> bool {
        self.state.lock().closed
    }

    pub(super) fn close(&self) {
        let mut state = self.state.lock();
        state.closed = true;
        state.queue = BudgetedDeque::new(&self.memory);
        drop(state);
        self.wake_receivers();
    }

    pub(super) fn fail(&self, error: NotificationSubscriptionError) {
        self.state.lock().fail(error, &self.memory);
        self.wake_receivers();
    }

    pub(super) fn deliver(&self, process_id: i32, channel: &str, payload: &str) {
        let mut state = self.state.lock();
        if state.closed {
            return;
        }
        if state.queue.len().saturating_add(state.reserved) >= self.options.max_queued_notifications
        {
            state.fail(
                NotificationSubscriptionError::new(NotificationFailureKind::Backpressure),
                &self.memory,
            );
        } else {
            let pending = state.reserved.saturating_add(1);
            let outcome = state
                .queue
                .reserve(pending)
                .and_then(|()| {
                    BufferedNotification::copy(process_id, channel, payload, &self.memory)
                })
                .and_then(|value| state.queue.push_back(value));
            if let Err(error) = outcome {
                state.fail(
                    NotificationSubscriptionError::with_source(
                        NotificationFailureKind::Backpressure,
                        error,
                    ),
                    &self.memory,
                );
            }
        }
        drop(state);
        self.wake_receivers();
    }

    fn wake_receivers(&self) {
        self.wake.notify_all();
        let waker = self.state.lock().async_waker.take();
        if let Some(waker) = waker {
            // Executor callbacks can reenter the inbox; never wake under its lock.
            waker.wake();
        }
    }

    pub(super) fn wait_async(&self) -> asynchronous::InboxWait<'_> {
        asynchronous::InboxWait::new(self)
    }

    pub(super) fn prepare(self: &Arc<Self>) -> PreparedNotifications {
        PreparedNotifications::new(self.clone())
    }

    pub(super) fn poll(
        &self,
    ) -> Result<Poll<Option<NotificationEvent>>, NotificationSubscriptionError> {
        self.state.lock().poll(&self.identity, &self.memory)
    }

    pub(super) fn wait(
        &self,
        timeout: Duration,
    ) -> Result<NotificationWait, NotificationSubscriptionError> {
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            NotificationSubscriptionError::new(NotificationFailureKind::InvalidRequest)
        })?;
        let mut state = self.state.lock();
        loop {
            match state.poll(&self.identity, &self.memory)? {
                Poll::Ready(Some(event)) => return Ok(NotificationWait::Event(event)),
                Poll::Ready(None) => return Ok(NotificationWait::Closed),
                Poll::Pending => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(NotificationWait::TimedOut);
            }
            self.wake.wait_for(&mut state, remaining);
        }
    }
}

impl InboxState {
    fn fail(&mut self, error: NotificationSubscriptionError, memory: &MemoryBudget) {
        if !self.closed {
            self.failure = Some(error);
            self.closed = true;
            self.queue = BudgetedDeque::new(memory);
        }
    }

    fn poll(
        &mut self,
        identity: &NotificationIdentity,
        memory: &MemoryBudget,
    ) -> Result<Poll<Option<NotificationEvent>>, NotificationSubscriptionError> {
        if let Some(error) = self.failure.as_ref() {
            return Err(error.clone());
        }
        if self.closed {
            return Ok(Poll::Ready(None));
        }
        let Some(sequence) = self.next_sequence else {
            let error =
                NotificationSubscriptionError::new(NotificationFailureKind::SequenceExhausted);
            self.fail(error.clone(), memory);
            return Err(error);
        };
        let Some(value) = self.queue.pop_front() else {
            return Ok(Poll::Pending);
        };
        self.next_sequence = sequence.checked_add(1);
        Ok(Poll::Ready(Some(NotificationEvent::Notification {
            identity: identity.clone(),
            sequence,
            notification: value.value,
        })))
    }
}

#[cfg(test)]
mod tests;
