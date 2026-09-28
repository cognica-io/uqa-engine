//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Delivery reservations stay private until the owning registry transaction commits.

use super::{
    Arc, BufferedNotification, NotificationFailureKind, NotificationSubscriptionError,
    SubscriptionInbox,
};
use uqa_core::memory::BudgetedVec;

pub(in crate::notifications) struct PreparedNotifications {
    inbox: Arc<SubscriptionInbox>,
    values: BudgetedVec<BufferedNotification>,
    reserved: usize,
}

impl PreparedNotifications {
    pub(super) fn new(inbox: Arc<SubscriptionInbox>) -> Self {
        Self {
            values: BudgetedVec::new(&inbox.memory),
            inbox,
            reserved: 0,
        }
    }

    pub(in crate::notifications) fn push(
        &mut self,
        process_id: i32,
        channel: &str,
        payload: &str,
    ) -> bool {
        let mut state = self.inbox.state.lock();
        if state.closed {
            return false;
        }
        if state.queue.len().saturating_add(state.reserved)
            >= self.inbox.options.max_queued_notifications
        {
            state.fail(
                NotificationSubscriptionError::new(NotificationFailureKind::Backpressure),
                &self.inbox.memory,
            );
            drop(state);
            self.inbox.wake_receivers();
            return false;
        }
        let pending = state.reserved.saturating_add(1);
        let result = state
            .queue
            .reserve(pending)
            .and_then(|()| self.values.reserve(1))
            .and_then(|()| {
                BufferedNotification::copy(process_id, channel, payload, &self.inbox.memory)
            })
            .and_then(|value| self.values.push(value));
        if let Err(error) = result {
            state.fail(
                NotificationSubscriptionError::with_source(
                    NotificationFailureKind::Backpressure,
                    error,
                ),
                &self.inbox.memory,
            );
            drop(state);
            self.inbox.wake_receivers();
            return false;
        }
        state.reserved += 1;
        self.reserved += 1;
        true
    }

    pub(in crate::notifications) fn commit(mut self) {
        let values = std::mem::replace(&mut self.values, BudgetedVec::new(&self.inbox.memory));
        let (values, _memory) = values.into_parts();
        let mut state = self.inbox.state.lock();
        state.reserved -= std::mem::take(&mut self.reserved);
        for value in values {
            if !state.closed {
                if let Err(error) = state.queue.push_back(value) {
                    state.fail(
                        NotificationSubscriptionError::with_source(
                            NotificationFailureKind::Backpressure,
                            error,
                        ),
                        &self.inbox.memory,
                    );
                }
            }
        }
        drop(state);
        self.inbox.wake_receivers();
    }
}

impl Drop for PreparedNotifications {
    fn drop(&mut self) {
        self.inbox.state.lock().reserved -= self.reserved;
    }
}
