//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lossless intermediate values; the public JavaScript facade freezes and redacts them.

use napi::bindgen_prelude::BigInt;
use napi_derive::napi;
use uqa_core::notifications::{NotificationEvent, NotificationIdentity};

#[napi(object)]
pub struct NativeNotificationIdentity {
    pub epoch: String,
    pub request_id: Option<String>,
}

impl From<&NotificationIdentity> for NativeNotificationIdentity {
    fn from(value: &NotificationIdentity) -> Self {
        Self {
            epoch: value.epoch.to_string(),
            request_id: value.request_id.as_ref().map(ToString::to_string),
        }
    }
}

#[napi(object)]
pub struct NativeNotificationEvent {
    pub kind: String,
    pub epoch: String,
    pub request_id: Option<String>,
    pub sequence: Option<BigInt>,
    pub process_id: Option<i32>,
    pub channel: Option<String>,
    pub payload: Option<String>,
    pub cause: Option<String>,
}

impl From<NotificationEvent> for NativeNotificationEvent {
    fn from(value: NotificationEvent) -> Self {
        let (kind, identity, sequence, notification, cause) = match value {
            NotificationEvent::Notification {
                identity,
                sequence,
                notification,
            } => (
                "notification",
                identity,
                Some(sequence),
                Some(notification),
                None,
            ),
            NotificationEvent::ResyncRequired { identity, cause } => (
                "resync_required",
                identity,
                None,
                None,
                Some(cause.code().to_owned()),
            ),
            NotificationEvent::Reconnected { identity } => {
                ("reconnected", identity, None, None, None)
            }
        };
        let (process_id, channel, payload) = notification.map_or((None, None, None), |value| {
            (
                Some(value.process_id),
                Some(value.channel),
                Some(value.payload),
            )
        });
        Self {
            kind: kind.to_owned(),
            epoch: identity.epoch.to_string(),
            request_id: identity.request_id.map(|value| value.to_string()),
            sequence: sequence.map(|value| BigInt {
                sign_bit: false,
                words: vec![value],
            }),
            process_id,
            channel,
            payload,
            cause,
        }
    }
}
