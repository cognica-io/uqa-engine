//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound JavaScript input before copying and preserve exact Unicode channel names.

use napi::bindgen_prelude::{Array, Result};
use napi::JsString;
use napi_derive::napi;
use uqa_core::notifications::NotificationFailureKind as Kind;
use uqa_engine::NotificationSubscriptionOptions;

#[napi(object)]
pub struct NativeNotificationOptions {
    #[napi(js_name = "maxActiveSubscriptions")]
    pub active_subscriptions: f64,
    #[napi(js_name = "maxChannels")]
    pub channels: f64,
    #[napi(js_name = "maxQueuedNotifications")]
    pub queued_notifications: f64,
    #[napi(js_name = "maxQueuedBytes")]
    pub queued_bytes: f64,
    #[napi(js_name = "maxRegistryEntriesPerPoll")]
    pub registry_entries_per_poll: f64,
}

fn positive(value: f64) -> Result<usize> {
    crate::input::positive_safe_usize(value).ok_or_else(|| super::failure(Kind::InvalidRequest))
}

impl NativeNotificationOptions {
    pub(super) fn validate(self) -> Result<NotificationSubscriptionOptions> {
        Ok(NotificationSubscriptionOptions {
            max_active_subscriptions: positive(self.active_subscriptions)?,
            max_channels: positive(self.channels)?,
            max_queued_notifications: positive(self.queued_notifications)?,
            max_queued_bytes: positive(self.queued_bytes)?,
            max_registry_entries_per_poll: positive(self.registry_entries_per_poll)?,
        })
    }
}

pub(super) fn channels(input: Array<'_>, maximum: usize) -> Result<Vec<String>> {
    let count = input.len() as usize;
    if count == 0 || count > maximum {
        return Err(super::failure(Kind::InvalidRequest));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(count)
        .map_err(|_| super::failure(Kind::Capacity))?;
    for index in 0..input.len() {
        let value = input
            .get::<JsString<'_>>(index)
            .map_err(|_| super::failure(Kind::InvalidRequest))?
            .ok_or_else(|| super::failure(Kind::InvalidRequest))?;
        if value.utf16_len()? > 63 {
            return Err(super::failure(Kind::InvalidRequest));
        }
        let channel = value
            .into_utf16()?
            .as_str()
            .map_err(|_| super::failure(Kind::InvalidRequest))?;
        if channel.is_empty() || channel.len() > 63 || channel.contains('\0') {
            return Err(super::failure(Kind::InvalidRequest));
        }
        output.push(channel);
    }
    Ok(output)
}
