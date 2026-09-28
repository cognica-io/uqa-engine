//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{HttpNotificationError, HttpNotificationOptions};
use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard},
};
use tokio::sync::Notify;
use uqa_core::notifications::{NotificationEvent, NotificationFailureKind};

pub(super) struct Inbox {
    state: Mutex<State>,
    pub changed: Notify,
    maximum_count: usize,
    maximum_bytes: usize,
}

struct State {
    events: VecDeque<NotificationEvent>,
    charged: usize,
    terminal: Option<Result<(), HttpNotificationError>>,
}

impl Inbox {
    pub fn new(options: &HttpNotificationOptions) -> Result<Self, HttpNotificationError> {
        if options
            .max_queued_events
            .checked_mul(size_of::<NotificationEvent>())
            .is_none_or(|bytes| bytes > options.max_queued_bytes)
        {
            return Err(capacity());
        }
        let mut events = VecDeque::new();
        events
            .try_reserve_exact(options.max_queued_events)
            .map_err(|_| capacity())?;
        let charged = events
            .capacity()
            .checked_mul(size_of::<NotificationEvent>())
            .ok_or_else(capacity)?;
        if charged > options.max_queued_bytes {
            return Err(capacity());
        }
        Ok(Self {
            state: Mutex::new(State {
                events,
                charged,
                terminal: None,
            }),
            changed: Notify::new(),
            maximum_count: options.max_queued_events,
            maximum_bytes: options.max_queued_bytes,
        })
    }

    pub fn push(&self, event: NotificationEvent) -> Result<(), HttpNotificationError> {
        let mut state = self.lock();
        if let Some(result) = &state.terminal {
            return Err(result
                .clone()
                .err()
                .unwrap_or_else(HttpNotificationError::cancelled));
        }
        let charged = state
            .charged
            .checked_add(dynamic_bytes(&event))
            .filter(|bytes| *bytes <= self.maximum_bytes);
        if state.events.len() == self.maximum_count || charged.is_none() {
            return Err(HttpNotificationError::local(
                NotificationFailureKind::Backpressure,
            ));
        }
        state.charged = charged.unwrap();
        state.events.push_back(event);
        drop(state);
        self.changed.notify_one();
        Ok(())
    }

    pub fn take(&self) -> Option<Result<Option<NotificationEvent>, HttpNotificationError>> {
        let mut state = self.lock();
        if let Some(event) = state.events.pop_front() {
            state.charged -= dynamic_bytes(&event);
            if state.events.is_empty() && state.terminal.is_some() {
                state.events = VecDeque::new();
                state.charged = 0;
            }
            return Some(Ok(Some(event)));
        }
        state.terminal.clone().map(|result| result.map(|()| None))
    }

    pub fn finish(&self, result: Result<(), HttpNotificationError>) {
        let mut state = self.lock();
        if state.events.is_empty()
            || result.as_ref().err().is_none_or(|error| {
                matches!(
                    error.kind(),
                    NotificationFailureKind::Cancelled
                        | NotificationFailureKind::Authentication
                        | NotificationFailureKind::AuthorityRevoked
                )
            })
        {
            state.events = VecDeque::new();
            state.charged = 0;
        }
        if state.terminal.is_none() {
            state.terminal = Some(result);
        }
        drop(state);
        self.changed.notify_waiters();
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn capacity() -> HttpNotificationError {
    HttpNotificationError::local(NotificationFailureKind::Capacity)
}

fn dynamic_bytes(event: &NotificationEvent) -> usize {
    let (identity, strings) = match event {
        NotificationEvent::Notification {
            identity,
            notification,
            ..
        } => (
            identity,
            notification
                .channel
                .capacity()
                .saturating_add(notification.payload.capacity()),
        ),
        NotificationEvent::ResyncRequired { identity, .. }
        | NotificationEvent::Reconnected { identity } => (identity, 0),
    };
    strings.saturating_add(
        identity
            .request_id
            .as_ref()
            .map_or(0, |id| id.as_str().len()),
    )
}
