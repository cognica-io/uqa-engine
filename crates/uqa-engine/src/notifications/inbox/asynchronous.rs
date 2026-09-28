//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One cancellation-safe executor wake slot over the original bounded inbox.

use super::{
    NotificationEvent, NotificationFailureKind, NotificationSubscriptionError, SubscriptionInbox,
};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

pub(in crate::notifications) struct InboxWait<'a> {
    inbox: &'a SubscriptionInbox,
    registered: bool,
}

impl<'a> InboxWait<'a> {
    pub(super) fn new(inbox: &'a SubscriptionInbox) -> Self {
        Self {
            inbox,
            registered: false,
        }
    }
}

impl Future for InboxWait<'_> {
    type Output = Result<Option<NotificationEvent>, NotificationSubscriptionError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        // Both clone and destruction belong outside the state lock: a custom
        // executor's RawWaker callbacks may themselves inspect this inbox.
        let next_waker = context.waker().clone();
        let mut state = this.inbox.state.lock();
        if !this.registered && state.async_waiting {
            return Poll::Ready(Err(NotificationSubscriptionError::new(
                NotificationFailureKind::InvalidRequest,
            )));
        }
        let result = match state.poll(&this.inbox.identity, &this.inbox.memory) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(event)) => Poll::Ready(Ok(event)),
            Err(error) => Poll::Ready(Err(error)),
        };
        let previous = if result.is_pending() {
            state.async_waiting = true;
            this.registered = true;
            state.async_waker.replace(next_waker)
        } else {
            state.async_waiting = false;
            this.registered = false;
            state.async_waker.take()
        };
        drop(state);
        drop(previous);
        result
    }
}

impl Drop for InboxWait<'_> {
    fn drop(&mut self) {
        if self.registered {
            let mut state = self.inbox.state.lock();
            state.async_waiting = false;
            let waker = state.async_waker.take();
            drop(state);
            drop(waker);
        }
    }
}
