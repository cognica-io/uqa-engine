//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Closed flat schemas, checked before any decoder state advances.

use super::super::{
    framing::Fields, json, timing::decimal, NotificationReady, NotificationTiming,
    NotificationWireEvent, ProtocolError, ServerFailure, SubscriptionRequest, TimerLimits,
    MAX_NOTIFICATION_WIRE_BYTES,
};
use serde::{de::DeserializeOwned, Deserialize};
use uqa_core::notifications::{
    NotificationEpoch, NotificationEvent, NotificationIdentity, NotificationRequestId,
    SQLNotification,
};

pub(super) fn admit(
    fields: &Fields<'_>,
    request: &SubscriptionRequest,
    expected_request_id: &NotificationRequestId,
    timer_limits: TimerLimits,
    ready: Option<&NotificationReady>,
    last_sequence: u64,
) -> Result<NotificationWireEvent, ProtocolError> {
    if !matches!(fields.event, "ready" | "notification" | "error" | "closed") {
        return Err(ProtocolError::InvalidFields);
    }
    if fields.event == "ready" {
        if ready.is_some() {
            return Err(ProtocolError::EventOrder);
        }
        let raw: Ready = object(&fields.data)?;
        if raw.protocol_version != 1 {
            return Err(ProtocolError::UnsupportedVersion);
        }
        if raw.accepted_channel_count != request.channels().len() as u64
            || raw.delivery != "live"
            || raw.resume_supported
            || raw.max_event_bytes != MAX_NOTIFICATION_WIRE_BYTES as u64
        {
            return Err(ProtocolError::InvalidFields);
        }
        let identity = identity(&raw.request_id, &raw.stream_id, expected_request_id, None)?;
        let timing = NotificationTiming::new(
            decimal(&raw.heartbeat_interval_ms).ok_or(ProtocolError::Timing)?,
            decimal(&raw.idle_timeout_ms).ok_or(ProtocolError::Timing)?,
            decimal(&raw.timing_margin_ms).ok_or(ProtocolError::Timing)?,
            timer_limits,
        )?;
        return Ok(NotificationWireEvent::Ready(NotificationReady {
            identity,
            accepted_channel_count: request.channels().len(),
            timing,
        }));
    }
    let ready = ready.ok_or(ProtocolError::EventOrder)?;
    match fields.event {
        "notification" => {
            let raw: Notification = object(&fields.data)?;
            let identity = identity(
                &raw.request_id,
                &raw.stream_id,
                expected_request_id,
                Some(ready.identity.epoch),
            )?;
            let sequence = decimal(&raw.sequence).ok_or(ProtocolError::Sequence)?;
            if last_sequence.checked_add(1) != Some(sequence) {
                return Err(ProtocolError::Sequence);
            }
            if !request.contains(&raw.channel) {
                return Err(ProtocolError::InvalidChannels);
            }
            if raw.payload.len() > 7_999 {
                return Err(ProtocolError::Payload);
            }
            Ok(NotificationWireEvent::Notification(
                NotificationEvent::Notification {
                    identity,
                    sequence,
                    notification: SQLNotification {
                        process_id: raw.process_id,
                        channel: raw.channel,
                        payload: raw.payload,
                    },
                },
            ))
        }
        "error" => {
            let raw: Failure = object(&fields.data)?;
            let identity = identity(
                &raw.request_id,
                &raw.stream_id,
                expected_request_id,
                Some(ready.identity.epoch),
            )?;
            Ok(NotificationWireEvent::Error(ServerFailure::new(
                identity,
                raw.code,
                raw.retryable,
            )?))
        }
        "closed" => {
            let raw: Closed = object(&fields.data)?;
            let identity = identity(
                &raw.request_id,
                &raw.stream_id,
                expected_request_id,
                Some(ready.identity.epoch),
            )?;
            if raw.reason != "server_draining" {
                return Err(ProtocolError::InvalidFields);
            }
            Ok(NotificationWireEvent::ServerDraining { identity })
        }
        _ => Err(ProtocolError::InvalidFields),
    }
}

fn object<T: DeserializeOwned>(input: &str) -> Result<T, ProtocolError> {
    let input = json::validate(input.as_bytes())?;
    serde_json::from_str(input).map_err(|_| ProtocolError::InvalidFields)
}

fn identity(
    request_id: &str,
    stream_id: &str,
    expected_request_id: &NotificationRequestId,
    expected_epoch: Option<NotificationEpoch>,
) -> Result<NotificationIdentity, ProtocolError> {
    if request_id != expected_request_id.as_str() {
        return Err(ProtocolError::Identity);
    }
    let epoch: NotificationEpoch = stream_id.parse().map_err(|_| ProtocolError::Identity)?;
    if expected_epoch.is_some_and(|expected| expected != epoch) {
        return Err(ProtocolError::Identity);
    }
    Ok(NotificationIdentity {
        epoch,
        request_id: Some(expected_request_id.clone()),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    protocol_version: u64,
    request_id: String,
    stream_id: String,
    accepted_channel_count: u64,
    delivery: String,
    resume_supported: bool,
    max_event_bytes: u64,
    heartbeat_interval_ms: String,
    idle_timeout_ms: String,
    timing_margin_ms: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Notification {
    request_id: String,
    stream_id: String,
    sequence: String,
    process_id: i32,
    channel: String,
    payload: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Failure {
    request_id: String,
    stream_id: String,
    code: String,
    retryable: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Closed {
    request_id: String,
    stream_id: String,
    reason: String,
}
