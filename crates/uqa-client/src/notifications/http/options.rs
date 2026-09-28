//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::HttpNotificationError;
use crate::notifications::{TimerLimits, MAX_NOTIFICATION_WIRE_BYTES};
use std::{
    num::{NonZeroU64, NonZeroUsize},
    time::Duration,
};
use tokio::time::Instant;

/// Native client timer range: one full period of Tokio's six-level, six-bit millisecond wheel. This is a supported range, not a deployment timeout default.
pub(super) const MAX_TIMER_MS: u64 = (1 << 36) - 1;

/// Required caller budgets. No capacity or deployment timeout is inferred from SQL settings.
#[derive(Clone, Debug)]
pub struct HttpNotificationOptions {
    pub max_channels: usize,
    pub max_queued_events: usize,
    pub max_queued_bytes: usize,
    /// Bounds the one HTTP data chunk retained by the worker, separately from the per-frame wire limit.
    pub max_transport_chunk_bytes: usize,
    pub connect_timeout: Duration,
    /// Bounds the entire initial attempt through ready, including connection and registration.
    pub ready_timeout: Duration,
    /// Reject a server's incompatible idle budget before exposing ready.
    pub max_idle_timeout: Duration,
    /// None explicitly disables reconnection. A supplied policy manages each post-ready loss episode.
    pub retry: Option<NotificationRetryOptions>,
}

#[derive(Clone, Debug)]
pub struct NotificationRetryOptions {
    /// Replacement attempts in one loss episode; the initial ready stream is not an attempt in this count.
    pub max_attempts: u32,
    pub episode_timeout: Duration,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub max_retry_after: Duration,
}

impl HttpNotificationOptions {
    pub(super) fn validate(&self) -> Result<(), HttpNotificationError> {
        if self.max_channels == 0
            || self.max_queued_events == 0
            || self.max_queued_bytes == 0
            || self.max_transport_chunk_bytes < MAX_NOTIFICATION_WIRE_BYTES
        {
            return Err(HttpNotificationError::invalid_options());
        }
        for duration in [
            self.connect_timeout,
            self.ready_timeout,
            self.max_idle_timeout,
        ] {
            validate_duration(duration)?;
        }
        if self.connect_timeout > self.ready_timeout {
            return Err(HttpNotificationError::invalid_options());
        }
        if let Some(retry) = &self.retry {
            if retry.max_attempts == 0 || retry.initial_backoff > retry.max_backoff {
                return Err(HttpNotificationError::invalid_options());
            }
            for duration in [
                retry.episode_timeout,
                retry.initial_backoff,
                retry.max_backoff,
                retry.max_retry_after,
            ] {
                validate_duration(duration)?;
            }
        }
        Ok(())
    }

    pub(super) fn channel_limit(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.max_channels).expect("validated channel limit")
    }

    pub(super) fn timer_limits(&self) -> TimerLimits {
        TimerLimits::new(
            NonZeroU64::new(MAX_TIMER_MS).unwrap(),
            NonZeroU64::new(self.max_idle_timeout.as_millis() as u64),
        )
    }
}

fn validate_duration(duration: Duration) -> Result<(), HttpNotificationError> {
    if duration.is_zero()
        || duration.as_millis() == 0
        || duration.as_millis() > u128::from(MAX_TIMER_MS)
        || !duration.subsec_nanos().is_multiple_of(1_000_000)
        || Instant::now().checked_add(duration).is_none()
    {
        return Err(HttpNotificationError::invalid_options());
    }
    Ok(())
}

pub(super) fn deadline(duration: Duration) -> Result<Instant, HttpNotificationError> {
    Instant::now()
        .checked_add(duration)
        .ok_or_else(HttpNotificationError::invalid_options)
}
