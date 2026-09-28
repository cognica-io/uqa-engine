//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact millisecond validation, separate from clock scheduling and qualified policy selection.

use super::ProtocolError;
use std::num::NonZeroU64;

/// Supplied by the receiving runtime and its configured deadline policy; the wire protocol invents no timer default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerLimits {
    maximum_timer_ms: NonZeroU64,
    maximum_idle_ms: Option<NonZeroU64>,
}

impl TimerLimits {
    pub const fn new(maximum_timer_ms: NonZeroU64, maximum_idle_ms: Option<NonZeroU64>) -> Self {
        Self {
            maximum_timer_ms,
            maximum_idle_ms,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationTiming {
    heartbeat_interval: u64,
    idle_timeout: u64,
    timing_margin: u64,
}

impl NotificationTiming {
    pub fn new(
        heartbeat_interval_ms: u64,
        idle_timeout_ms: u64,
        timing_margin_ms: u64,
        limits: TimerLimits,
    ) -> Result<Self, ProtocolError> {
        if [heartbeat_interval_ms, idle_timeout_ms, timing_margin_ms].contains(&0) {
            return Err(ProtocolError::Timing);
        }
        if [heartbeat_interval_ms, idle_timeout_ms, timing_margin_ms]
            .into_iter()
            .any(|value| value > limits.maximum_timer_ms.get())
            || limits
                .maximum_idle_ms
                .is_some_and(|limit| idle_timeout_ms > limit.get())
        {
            return Err(ProtocolError::TimerRange);
        }
        let minimum = heartbeat_interval_ms
            .checked_mul(3)
            .and_then(|value| value.checked_add(timing_margin_ms))
            .ok_or(ProtocolError::Timing)?;
        if idle_timeout_ms <= minimum {
            return Err(ProtocolError::Timing);
        }
        Ok(Self {
            heartbeat_interval: heartbeat_interval_ms,
            idle_timeout: idle_timeout_ms,
            timing_margin: timing_margin_ms,
        })
    }

    pub const fn heartbeat_interval_ms(self) -> u64 {
        self.heartbeat_interval
    }
    pub const fn idle_timeout_ms(self) -> u64 {
        self.idle_timeout
    }
    pub const fn timing_margin_ms(self) -> u64 {
        self.timing_margin
    }
}

pub(super) fn decimal(value: &str) -> Option<u64> {
    if value.is_empty()
        || value.len() > 20
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse().ok()
}
