//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

mod framing;
mod request;
mod validation;

use super::*;
use serde::Deserialize;
use serde_json::Value;
use std::{
    num::{NonZeroU64, NonZeroUsize},
    sync::Arc,
};
use uqa_core::notifications::{NotificationEvent, NotificationRequestId};

#[derive(Deserialize)]
struct Fixture {
    protocol_version: u64,
    request_id: String,
    stream_id: String,
    channels: Vec<String>,
    valid_request: String,
    invalid_nested_request: String,
    ready: String,
    notification: String,
    expected_payload: String,
    closed: String,
    error: String,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("../../tests/fixtures/notifications-v1.json")).unwrap()
}

fn maximum_channels() -> NonZeroUsize {
    NonZeroUsize::new(1_000).unwrap()
}

fn timer_limits() -> TimerLimits {
    TimerLimits::new(NonZeroU64::new(u64::MAX).unwrap(), None)
}

fn decoder_with_limits(limits: TimerLimits) -> NotificationDecoder {
    let fixture = fixture();
    NotificationDecoder::new(
        Arc::new(
            SubscriptionRequest::from_json(
                fixture.valid_request.as_bytes(),
                maximum_channels(),
                None,
            )
            .unwrap(),
        ),
        NotificationRequestId::new(&fixture.request_id).unwrap(),
        limits,
    )
    .unwrap()
}

pub(in crate::notifications) fn decoder() -> NotificationDecoder {
    decoder_with_limits(timer_limits())
}

pub(in crate::notifications) fn ready_decoder() -> NotificationDecoder {
    let mut decoder = decoder();
    assert!(matches!(
        decoder.decode(fixture().ready.as_bytes()).unwrap().event,
        Some(NotificationWireEvent::Ready(_))
    ));
    decoder
}

fn body(frame: &str) -> Value {
    serde_json::from_str(frame.split_once("data: ").unwrap().1.trim_end()).unwrap()
}

pub(in crate::notifications) fn event(event: &str, body: &Value) -> Vec<u8> {
    format!("event: {event}\ndata: {body}\n\n").into_bytes()
}

pub(in crate::notifications) fn notification_body() -> Value {
    body(&fixture().notification)
}

fn feed(
    decoder: &mut NotificationDecoder,
    input: &[u8],
    output: &mut Vec<NotificationWireEvent>,
) -> Result<(), ProtocolError> {
    let mut offset = 0;
    while offset < input.len() {
        let step = decoder.decode(&input[offset..])?;
        assert!(step.consumed <= input.len() - offset);
        assert!(step.consumed != 0 || step.event.is_some());
        offset += step.consumed;
        output.extend(step.event);
    }
    Ok(())
}

fn fail_frame(frame: &[u8], error: ProtocolError, after_ready: bool) {
    let mut decoder = if after_ready {
        ready_decoder()
    } else {
        decoder()
    };
    let mut output = Vec::new();
    assert_eq!(feed(&mut decoder, frame, &mut output), Err(error));
    assert_eq!(output.len(), 0);
    assert!(decoder.is_terminal());
    assert_eq!(
        decoder.decode(fixture().ready.as_bytes()).unwrap_err(),
        error
    );
    assert_eq!(decoder.finish(), Err(error));
}

#[test]
fn independent_fixture_preserves_exact_values_and_terminal_identity() {
    let fixture = fixture();
    assert_eq!(fixture.protocol_version, 1);
    let mut decoder = decoder();
    let mut output = Vec::new();
    for input in [&fixture.ready, &fixture.notification, &fixture.closed] {
        feed(&mut decoder, input.as_bytes(), &mut output).unwrap();
    }
    assert_eq!(output.len(), 3);
    let NotificationWireEvent::Ready(ready) = &output[0] else {
        panic!("ready")
    };
    assert_eq!(
        ready.identity.request_id.as_ref().unwrap().as_str(),
        fixture.request_id
    );
    assert_eq!(ready.identity.epoch.to_string(), fixture.stream_id);
    assert_eq!(ready.accepted_channel_count, fixture.channels.len());
    assert_eq!(ready.timing.heartbeat_interval_ms(), 100);
    assert_eq!(ready.timing.idle_timeout_ms(), 401);
    assert_eq!(ready.timing.timing_margin_ms(), 100);
    let NotificationWireEvent::Notification(NotificationEvent::Notification {
        identity,
        sequence,
        notification,
    }) = &output[1]
    else {
        panic!("notification")
    };
    assert_eq!(identity, &ready.identity);
    assert_eq!(*sequence, 1);
    assert_eq!(notification.process_id, i32::MIN);
    assert_eq!(notification.channel, "작업");
    assert_eq!(notification.payload, fixture.expected_payload);
    assert!(
        matches!(&output[2], NotificationWireEvent::ServerDraining { identity } if identity == &ready.identity)
    );
    assert!(decoder.is_terminal());
    assert_eq!(decoder.finish().unwrap(), None);
}

#[test]
fn diagnostics_never_format_channels_payloads_or_rejected_input() {
    let secret = "PRIVATE_SENTINEL_6UQ";
    let request = SubscriptionRequest::new(&[secret], maximum_channels()).unwrap();
    assert!(!format!("{request:?}").contains(secret));
    let mut notification = notification_body();
    notification["payload"] = Value::String(secret.into());
    let step = ready_decoder()
        .decode(&event("notification", &notification))
        .unwrap();
    assert!(!format!("{step:?}").contains(secret));
    let mut failure = body(&fixture().error);
    failure["code"] = Value::String(secret.into());
    let step = ready_decoder().decode(&event("error", &failure)).unwrap();
    assert!(!format!("{step:?}").contains(secret));
    let Some(NotificationWireEvent::Error(error)) = step.event else {
        panic!("server failure")
    };
    assert_eq!(error.code(), secret);
    assert_eq!(error.known_kind(), None);
    for input in [
        format!("event: {secret}\ndata: {{}}\n\n"),
        format!("event: notification\ndata: {{\"{secret}\":\"{secret}\"}}\n\n"),
    ] {
        let error = ready_decoder().decode(input.as_bytes()).unwrap_err();
        assert!(!format!("{error:?} {error}").contains(secret));
    }
}
