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
    NotificationReceive, NotificationSubscriptionError, NotificationSubscriptionOptions,
    NotificationWait,
};

pub(crate) struct SubscriptionInbox {
    pub(super) options: NotificationSubscriptionOptions,
    pub(super) memory: MemoryBudget,
    identity: NotificationIdentity,
    pub(in crate::notifications) state: Mutex<InboxState>,
    pub(in crate::notifications) wake: Condvar,
    #[cfg(test)]
    pub(in crate::notifications) sql_wait_started: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

pub(in crate::notifications) struct InboxState {
    pub(in crate::notifications) queue: BudgetedDeque<BufferedNotification>,
    reserved: usize,
    next_sequence: Option<u64>,
    pub(in crate::notifications) closed: bool,
    pub(in crate::notifications) failure: Option<NotificationSubscriptionError>,
    async_waiting: bool,
    async_waker: Option<std::task::Waker>,
}

pub(in crate::notifications) struct BufferedNotification {
    pub(in crate::notifications) value: SQLNotification,
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
            #[cfg(test)]
            sql_wait_started: Mutex::new(None),
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
        Ok(
            match self.wait_with(timeout, |_| Ok::<_, NotificationSubscriptionError>(()))? {
                NotificationReceive::Event { event, .. } => NotificationWait::Event(event),
                NotificationReceive::TimedOut => NotificationWait::TimedOut,
                NotificationReceive::Closed => NotificationWait::Closed,
            },
        )
    }

    pub(super) fn poll_with<T, E: From<NotificationSubscriptionError>>(
        &self,
        prepare: impl FnOnce(&SQLNotification) -> Result<T, E>,
    ) -> Result<NotificationReceive<T>, E> {
        self.state
            .lock()
            .receive_with(&self.identity, &self.memory, prepare)
    }

    pub(super) fn wait_with<T, E: From<NotificationSubscriptionError>>(
        &self,
        timeout: Duration,
        prepare: impl FnOnce(&SQLNotification) -> Result<T, E>,
    ) -> Result<NotificationReceive<T>, E> {
        let started = Instant::now();
        let mut prepare = Some(prepare);
        let mut state = self.state.lock();
        loop {
            match state.receive_with(&self.identity, &self.memory, |notification| {
                prepare.take().expect("one admitted event ends the receive")(notification)
            })? {
                NotificationReceive::TimedOut => {}
                received => return Ok(received),
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(NotificationReceive::TimedOut);
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
        Ok(
            match self.receive_with(identity, memory, |_| {
                Ok::<_, NotificationSubscriptionError>(())
            })? {
                NotificationReceive::Event { event, .. } => Poll::Ready(Some(event)),
                NotificationReceive::Closed => Poll::Ready(None),
                NotificationReceive::TimedOut => Poll::Pending,
            },
        )
    }

    fn receive_with<T, E: From<NotificationSubscriptionError>>(
        &mut self,
        identity: &NotificationIdentity,
        memory: &MemoryBudget,
        prepare: impl FnOnce(&SQLNotification) -> Result<T, E>,
    ) -> Result<NotificationReceive<T>, E> {
        if let Some(error) = self.failure.as_ref() {
            return Err(error.clone().into());
        }
        if self.closed {
            return Ok(NotificationReceive::Closed);
        }
        let Some(sequence) = self.next_sequence else {
            let error =
                NotificationSubscriptionError::new(NotificationFailureKind::SequenceExhausted);
            self.fail(error.clone(), memory);
            return Err(error.into());
        };
        let Some(value) = self.queue.iter().next() else {
            return Ok(NotificationReceive::TimedOut);
        };
        let admitted = prepare(&value.value)?;
        let identity = identity.clone();
        let value = self
            .queue
            .pop_front()
            .expect("admitted front remains under the inbox lock");
        self.next_sequence = sequence.checked_add(1);
        Ok(NotificationReceive::Event {
            event: NotificationEvent::Notification {
                identity,
                sequence,
                notification: value.value,
            },
            admitted,
        })
    }
}

#[cfg(test)]
mod tests;
