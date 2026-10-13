//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Optional bounded output for LISTEN on the existing SQL session.

use super::{
    inbox::SubscriptionInbox, Engine, NotificationSubscriptionError,
    NotificationSubscriptionOptions, SQLError, SQLNotification,
};
use uqa_core::notifications::{NotificationEpoch, NotificationFailureKind, NotificationIdentity};

/// Opt-in bounds for the SQL session's own LISTEN queue. Overflow is terminal and explicit.
/// Queue bytes include string capacities, queue slots and transient prepared batches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SQLNotificationOptions {
    pub max_channels: usize,
    pub max_queued_notifications: usize,
    pub max_queued_bytes: usize,
    pub max_registry_entries_per_poll: usize,
}

impl Engine {
    /// Configure bounded LISTEN delivery once, before opening a transaction or listening.
    /// This does not create another session, change channels or execute SQL. The adapter
    /// must use `try_take_sql_notifications` and close this session on a terminal error.
    pub fn configure_sql_notifications(
        &self,
        options: SQLNotificationOptions,
    ) -> Result<(), NotificationSubscriptionError> {
        let options = NotificationSubscriptionOptions {
            max_active_subscriptions: 1,
            max_channels: options.max_channels,
            max_queued_notifications: options.max_queued_notifications,
            max_queued_bytes: options.max_queued_bytes,
            max_registry_entries_per_poll: options.max_registry_entries_per_poll,
        };
        options.validate()?;
        let _statement = self
            .runtime
            .statement_gate
            .lock_with_cancellation(&self.runtime.cancellation)
            .map_err(|error| {
                NotificationSubscriptionError::with_source(
                    NotificationFailureKind::Cancelled,
                    error,
                )
            })?;
        let mut selected = self.runtime.bounded_sql_notifications.lock();
        if selected.is_some()
            || self.transaction_depth() != 0
            || !self.listening_channels().is_empty()
            || !self.runtime.notifications.lock().is_empty()
        {
            return Err(NotificationSubscriptionError::new(
                NotificationFailureKind::InvalidRequest,
            ));
        }
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).map_err(|error| {
            NotificationSubscriptionError::with_source(
                NotificationFailureKind::SourceUnavailable,
                std::io::Error::other(error.to_string()),
            )
        })?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        *selected = Some(SubscriptionInbox::new(
            NotificationIdentity {
                epoch: NotificationEpoch::from_bytes(bytes).expect("set UUID version and variant"),
                request_id: None,
            },
            options,
        ));
        Ok(())
    }

    /// Synchronize committed messages and atomically drain the same SQL session's inbox.
    /// Transactional LISTEN, UNLISTEN, NOTIFY and savepoint semantics remain unchanged.
    /// An overflow or source failure remains terminal; no lost message is called success.
    pub fn try_take_sql_notifications(&self) -> Result<Vec<SQLNotification>, SQLError> {
        if self.transaction_depth() != 0 {
            return Ok(Vec::new());
        }
        self.poll_sql_notifications()?;
        let bounded = self.runtime.bounded_sql_notifications.lock().clone();
        if let Some(inbox) = bounded {
            return inbox.take_sql().map_err(|error| sql_failure(&error));
        }
        Ok(self.runtime.notifications.lock().drain(..).collect())
    }

    pub(super) fn validate_sql_notification_channels(
        &self,
        channels: &[String],
    ) -> Result<(), SQLError> {
        if self
            .runtime
            .bounded_sql_notifications
            .lock()
            .as_ref()
            .is_some_and(|inbox| channels.len() > inbox.options.max_channels)
        {
            return Err(SQLError::Routine {
                sqlstate: "54000".into(),
                message: "SQL notification channel limit exceeded".into(),
            });
        }
        Ok(())
    }
}

pub(super) fn sql_failure(error: &NotificationSubscriptionError) -> SQLError {
    SQLError::Internal(error.code().into())
}

impl SubscriptionInbox {
    pub(in crate::notifications) fn pending_count(
        &self,
    ) -> Result<usize, NotificationSubscriptionError> {
        let state = self.state.lock();
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        Ok(state.queue.len())
    }
    pub(in crate::notifications) fn take_sql(
        &self,
    ) -> Result<Vec<SQLNotification>, NotificationSubscriptionError> {
        let mut state = self.state.lock();
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        let mut result = Vec::with_capacity(state.queue.len());
        while let Some(value) = state.queue.pop_front() {
            result.push(value.value);
        }
        Ok(result)
    }
    pub(in crate::notifications) fn wait_ready(
        &self,
        timeout: std::time::Duration,
    ) -> Result<bool, NotificationSubscriptionError> {
        let started = std::time::Instant::now();
        let mut state = self.state.lock();
        loop {
            if let Some(error) = &state.failure {
                return Err(error.clone());
            }
            if !state.queue.is_empty() {
                return Ok(true);
            }
            if state.closed {
                return Ok(false);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(false);
            }
            #[cfg(test)]
            if let Some(started) = self.sql_wait_started.lock().take() {
                let _ = started.send(());
            }
            self.wake.wait_for(&mut state, remaining);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Duration};

    #[test]
    fn sql_wait_releases_configuration_ownership_before_parking_the_inbox() {
        let engine = Arc::new(Engine::new());
        engine
            .configure_sql_notifications(SQLNotificationOptions {
                max_channels: 1,
                max_queued_notifications: 2,
                max_queued_bytes: 65_536,
                max_registry_entries_per_poll: 2,
            })
            .unwrap();
        engine.sql("LISTEN events", &[]).unwrap();
        let inbox = engine
            .runtime
            .bounded_sql_notifications
            .lock()
            .clone()
            .unwrap();
        let (started, boundary) = std::sync::mpsc::channel();
        *inbox.sql_wait_started.lock() = Some(started);
        let waiting = engine.clone();
        let task =
            std::thread::spawn(move || waiting.wait_for_sql_notifications(Duration::from_secs(15)));
        boundary.recv_timeout(Duration::from_secs(15)).unwrap();
        let released = engine
            .runtime
            .bounded_sql_notifications
            .try_lock()
            .is_some();
        if !released {
            inbox.close();
            let _ = task.join();
            panic!("notification wait retained the configuration lock");
        }
        engine.sql("NOTIFY events, 'ready'", &[]).unwrap();
        assert!(task.join().unwrap().unwrap());
        assert_eq!(
            engine.try_take_sql_notifications().unwrap()[0].payload,
            "ready"
        );
    }
}
