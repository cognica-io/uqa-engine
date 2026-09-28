//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Required Python subscription allowances map directly to owning Rust options.

use pyo3::prelude::*;
use std::time::Duration;
use uqa_client::notifications::{HttpNotificationOptions, NotificationRetryOptions};
use uqa_engine::NotificationSubscriptionOptions;

#[pyclass(name = "NotificationSubscriptionOptions", module = "uqa._uqa", frozen)]
pub(crate) struct PyNotificationOptions(pub(super) NotificationSubscriptionOptions);

#[pymethods]
impl PyNotificationOptions {
    #[new]
    #[pyo3(signature = (*, max_active_subscriptions, max_channels, max_queued_notifications, max_queued_bytes, max_registry_entries_per_poll))]
    fn new(
        max_active_subscriptions: usize,
        max_channels: usize,
        max_queued_notifications: usize,
        max_queued_bytes: usize,
        max_registry_entries_per_poll: usize,
    ) -> Self {
        Self(NotificationSubscriptionOptions {
            max_active_subscriptions,
            max_channels,
            max_queued_notifications,
            max_queued_bytes,
            max_registry_entries_per_poll,
        })
    }
}

#[pyclass(name = "NotificationRetryOptions", module = "uqa._uqa", frozen)]
pub(crate) struct PyNotificationRetryOptions(pub(super) NotificationRetryOptions);

#[pymethods]
impl PyNotificationRetryOptions {
    #[new]
    #[pyo3(signature = (*, max_attempts, episode_timeout_ms, initial_backoff_ms, max_backoff_ms, max_retry_after_ms))]
    fn new(
        max_attempts: u32,
        episode_timeout_ms: u64,
        initial_backoff_ms: u64,
        max_backoff_ms: u64,
        max_retry_after_ms: u64,
    ) -> Self {
        Self(NotificationRetryOptions {
            max_attempts,
            episode_timeout: Duration::from_millis(episode_timeout_ms),
            initial_backoff: Duration::from_millis(initial_backoff_ms),
            max_backoff: Duration::from_millis(max_backoff_ms),
            max_retry_after: Duration::from_millis(max_retry_after_ms),
        })
    }
}

#[pyclass(name = "HttpNotificationOptions", module = "uqa._uqa", frozen)]
pub(crate) struct PyHttpNotificationOptions(pub(super) HttpNotificationOptions);

#[pymethods]
impl PyHttpNotificationOptions {
    #[new]
    #[pyo3(signature = (*, max_channels, max_queued_events, max_queued_bytes, max_transport_chunk_bytes, connect_timeout_ms, ready_timeout_ms, max_idle_timeout_ms, retry=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        max_channels: usize,
        max_queued_events: usize,
        max_queued_bytes: usize,
        max_transport_chunk_bytes: usize,
        connect_timeout_ms: u64,
        ready_timeout_ms: u64,
        max_idle_timeout_ms: u64,
        retry: Option<PyRef<'_, PyNotificationRetryOptions>>,
    ) -> Self {
        Self(HttpNotificationOptions {
            max_channels,
            max_queued_events,
            max_queued_bytes,
            max_transport_chunk_bytes,
            connect_timeout: Duration::from_millis(connect_timeout_ms),
            ready_timeout: Duration::from_millis(ready_timeout_ms),
            max_idle_timeout: Duration::from_millis(max_idle_timeout_ms),
            retry: retry.map(|value| value.0.clone()),
        })
    }
}
