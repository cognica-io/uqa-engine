//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use serde_json::json;
use uqa_core::notifications::NotificationFailureKind;

#[test]
fn every_envelope_rejects_missing_unknown_and_duplicate_json_fields() {
    let fixture = fixture();
    for (kind, frame) in [
        ("ready", fixture.ready),
        ("notification", fixture.notification),
        ("error", fixture.error),
        ("closed", fixture.closed),
    ] {
        let original = body(&frame);
        let mut unexpected = original.clone();
        unexpected["unknown"] = json!("private data");
        fail_frame(
            &event(kind, &unexpected),
            ProtocolError::InvalidFields,
            kind != "ready",
        );
        for (key, value) in original.as_object().unwrap() {
            let mut missing = original.clone();
            missing.as_object_mut().unwrap().remove(key);
            fail_frame(
                &event(kind, &missing),
                ProtocolError::InvalidFields,
                kind != "ready",
            );
            let serialized = original.to_string();
            let duplicate = format!(
                "event: {kind}\ndata: {{{}: {value},{}\n\n",
                serde_json::to_string(key).unwrap(),
                &serialized[1..]
            );
            fail_frame(
                duplicate.as_bytes(),
                ProtocolError::InvalidFields,
                kind != "ready",
            );
        }
        let serialized = original.to_string();
        let escaped_duplicate = format!(
            "event: {kind}\ndata: {{\"request_\\u0069d\":\"request_1\",{}\n\n",
            &serialized[1..]
        );
        fail_frame(
            escaped_duplicate.as_bytes(),
            ProtocolError::InvalidFields,
            kind != "ready",
        );
    }
}

#[test]
fn ready_requires_the_negotiated_version_set_and_live_policy() {
    let ready = body(&fixture().ready);
    for (field, value, error) in [
        (
            "protocol_version",
            json!(2),
            ProtocolError::UnsupportedVersion,
        ),
        ("protocol_version", json!(1.0), ProtocolError::InvalidFields),
        ("protocol_version", json!("1"), ProtocolError::InvalidFields),
        (
            "accepted_channel_count",
            json!(1),
            ProtocolError::InvalidFields,
        ),
        (
            "accepted_channel_count",
            json!(2.0),
            ProtocolError::InvalidFields,
        ),
        (
            "max_event_bytes",
            json!(65_535),
            ProtocolError::InvalidFields,
        ),
        (
            "max_event_bytes",
            json!("65536"),
            ProtocolError::InvalidFields,
        ),
        ("delivery", json!("replay"), ProtocolError::InvalidFields),
        (
            "resume_supported",
            json!(true),
            ProtocolError::InvalidFields,
        ),
        (
            "resume_supported",
            json!("false"),
            ProtocolError::InvalidFields,
        ),
    ] {
        let mut invalid = ready.clone();
        invalid[field] = value;
        fail_frame(&event("ready", &invalid), error, false);
    }
}

#[test]
fn identities_remain_canonical_and_equal_in_every_frame() {
    let fixture = fixture();
    for (kind, frame) in [
        ("ready", &fixture.ready),
        ("notification", &fixture.notification),
        ("error", &fixture.error),
        ("closed", &fixture.closed),
    ] {
        for (field, value) in [
            ("request_id", ""),
            ("request_id", "another_request"),
            ("request_id", "request_1 "),
            ("stream_id", "7FB52B7F-BDCA-4DB2-9EE0-490F99857201"),
            ("stream_id", "7fb52b7f-bdca-1db2-9ee0-490f99857201"),
            ("stream_id", "7fb52b7f-bdca-4db2-7ee0-490f99857201"),
            ("stream_id", "not-a-uuid"),
        ] {
            let mut invalid = body(frame);
            invalid[field] = json!(value);
            fail_frame(
                &event(kind, &invalid),
                ProtocolError::Identity,
                kind != "ready",
            );
        }
        if kind != "ready" {
            let mut changed = body(frame);
            changed["stream_id"] = json!("7fb52b7f-bdca-4db2-9ee0-490f99857202");
            fail_frame(&event(kind, &changed), ProtocolError::Identity, true);
        }
    }
}

#[test]
fn notification_integer_widths_sequence_and_channel_are_exact() {
    let notification = notification_body();
    for sequence in [
        "",
        "0",
        "00",
        "01",
        "+1",
        "-1",
        "1.0",
        "1e0",
        " 1",
        "1 ",
        "١",
        "2",
        "18446744073709551616",
    ] {
        let mut invalid = notification.clone();
        invalid["sequence"] = json!(sequence);
        fail_frame(
            &event("notification", &invalid),
            ProtocolError::Sequence,
            true,
        );
    }
    for (field, value) in [
        ("sequence", json!(1)),
        ("process_id", json!(2_147_483_648_i64)),
        ("process_id", json!(-2_147_483_649_i64)),
        ("process_id", json!(1.0)),
        ("process_id", json!("1")),
        ("channel", json!(null)),
        ("payload", json!({"nested":true})),
    ] {
        let mut invalid = notification.clone();
        invalid[field] = value;
        fail_frame(
            &event("notification", &invalid),
            ProtocolError::InvalidFields,
            true,
        );
    }
    for channel in ["Jobs", "unknown", "작업 ", ""] {
        let mut invalid = notification.clone();
        invalid["channel"] = json!(channel);
        fail_frame(
            &event("notification", &invalid),
            ProtocolError::InvalidChannels,
            true,
        );
    }
    for process_id in [i32::MIN, -1, 0, i32::MAX] {
        let mut valid = notification.clone();
        valid["process_id"] = json!(process_id);
        let step = ready_decoder()
            .decode(&event("notification", &valid))
            .unwrap();
        let Some(NotificationWireEvent::Notification(NotificationEvent::Notification {
            notification,
            ..
        })) = step.event
        else {
            panic!("notification")
        };
        assert_eq!(notification.process_id, process_id);
    }
    let mut decoder = ready_decoder();
    decoder.decode(fixture().notification.as_bytes()).unwrap();
    assert_eq!(
        decoder
            .decode(fixture().notification.as_bytes())
            .unwrap_err(),
        ProtocolError::Sequence
    );
}

#[test]
fn payload_is_opaque_and_the_bound_applies_to_decoded_utf8_bytes() {
    for payload in [
        String::new(),
        "\0".repeat(7_999),
        format!("{}x", "한".repeat(2_666)),
        "{[\"x\"]} 😀\n".into(),
    ] {
        let mut notification = notification_body();
        notification["payload"] = json!(payload);
        let wire = event("notification", &notification);
        assert!(wire.len() < MAX_NOTIFICATION_WIRE_BYTES);
        let step = ready_decoder().decode(&wire).unwrap();
        let Some(NotificationWireEvent::Notification(NotificationEvent::Notification {
            notification,
            ..
        })) = step.event
        else {
            panic!("notification")
        };
        assert_eq!(notification.payload, payload);
    }
    for payload in ["x".repeat(8_000), "한".repeat(2_667)] {
        let mut notification = notification_body();
        notification["payload"] = json!(payload);
        fail_frame(
            &event("notification", &notification),
            ProtocolError::Payload,
            true,
        );
    }
    let raw = fixture()
        .notification
        .replace("{[\\\"문자\\\"]} : 😀\\n", "\\uD83D\\uDE00");
    let step = ready_decoder().decode(raw.as_bytes()).unwrap();
    let Some(NotificationWireEvent::Notification(NotificationEvent::Notification {
        notification,
        ..
    })) = step.event
    else {
        panic!("notification")
    };
    assert_eq!(notification.payload, "😀");
}

#[test]
fn terminal_schemas_are_closed_and_unknown_codes_never_become_retry_policy() {
    let fixture = fixture();
    let step = ready_decoder().decode(fixture.error.as_bytes()).unwrap();
    let Some(NotificationWireEvent::Error(error)) = step.event else {
        panic!("server error")
    };
    assert_eq!(
        error.known_kind(),
        Some(NotificationFailureKind::SourceUnavailable)
    );
    assert!(error.retryable);
    for code in ["", "lowercase", "NOTIFICATION-FAILURE", "BAD\nCODE", "비밀"] {
        let mut invalid = body(&fixture.error);
        invalid["code"] = json!(code);
        fail_frame(
            &event("error", &invalid),
            ProtocolError::InvalidFields,
            true,
        );
    }
    for length in [1, 64, 65] {
        let mut code = body(&fixture.error);
        code["code"] = json!("A".repeat(length));
        if length == 65 {
            fail_frame(&event("error", &code), ProtocolError::InvalidFields, true);
        } else {
            let step = ready_decoder().decode(&event("error", &code)).unwrap();
            let Some(NotificationWireEvent::Error(error)) = step.event else {
                panic!("server error")
            };
            assert_eq!(error.known_kind(), None);
        }
    }
    let mut invalid = body(&fixture.closed);
    invalid["reason"] = json!("complete");
    fail_frame(
        &event("closed", &invalid),
        ProtocolError::InvalidFields,
        true,
    );
    let mut invalid = body(&fixture.error);
    invalid["retryable"] = json!("true");
    fail_frame(
        &event("error", &invalid),
        ProtocolError::InvalidFields,
        true,
    );
}

#[test]
fn readiness_is_first_and_unique_and_terminals_stop_the_stream() {
    let fixture = fixture();
    for frame in [&fixture.notification, &fixture.error, &fixture.closed] {
        fail_frame(frame.as_bytes(), ProtocolError::EventOrder, false);
    }
    fail_frame(fixture.ready.as_bytes(), ProtocolError::EventOrder, true);
    for kind in ["reconnected", "ResyncRequired", "Notification", "unknown"] {
        fail_frame(
            &event(kind, &notification_body()),
            ProtocolError::InvalidFields,
            true,
        );
    }
    for terminal in [&fixture.closed, &fixture.error] {
        let mut decoder = ready_decoder();
        assert!(decoder.decode(terminal.as_bytes()).unwrap().event.is_some());
        assert!(decoder.is_terminal());
        assert_eq!(
            decoder.decode(fixture.notification.as_bytes()).unwrap_err(),
            ProtocolError::EventOrder
        );
    }
    let mut decoder = decoder();
    assert_eq!(
        decoder.decode(b": heartbeat\n\n").unwrap().event,
        Some(NotificationWireEvent::Heartbeat)
    );
    assert!(decoder.ready().is_none());
    assert!(decoder
        .decode(fixture.ready.as_bytes())
        .unwrap()
        .event
        .is_some());
}

#[test]
fn flat_schema_and_depth_limit_are_both_enforced() {
    for raw in [
        "{}{}",
        "[1]",
        "{\"payload\":{\"a\":{}}}",
        "{\"payload\":\"\\uD800\"}",
        "{\"payload\":\"\\x00\"}",
    ] {
        fail_frame(
            format!("event: notification\ndata: {raw}\n\n").as_bytes(),
            ProtocolError::InvalidJSON,
            true,
        );
    }
    let mut nested = notification_body();
    nested["payload"] = json!(["opaque"]);
    fail_frame(
        &event("notification", &nested),
        ProtocolError::InvalidFields,
        true,
    );
}

#[test]
fn timing_fields_require_canonical_checked_and_supported_milliseconds() {
    let ready = body(&fixture().ready);
    for field in [
        "heartbeat_interval_ms",
        "idle_timeout_ms",
        "timing_margin_ms",
    ] {
        for value in [
            "",
            "0",
            "01",
            "+1",
            "-1",
            "1.0",
            "1e3",
            " 100",
            "100 ",
            "١",
            "18446744073709551616",
        ] {
            let mut invalid = ready.clone();
            invalid[field] = json!(value);
            fail_frame(&event("ready", &invalid), ProtocolError::Timing, false);
        }
        let mut invalid = ready.clone();
        invalid[field] = json!(100);
        fail_frame(
            &event("ready", &invalid),
            ProtocolError::InvalidFields,
            false,
        );
    }
    let mut equal = ready.clone();
    equal["idle_timeout_ms"] = json!("400");
    fail_frame(&event("ready", &equal), ProtocolError::Timing, false);
    for (heartbeat, margin) in [(u64::MAX, 1), (1, u64::MAX)] {
        assert_eq!(
            NotificationTiming::new(heartbeat, u64::MAX, margin, timer_limits()),
            Err(ProtocolError::Timing)
        );
    }
    assert!(NotificationTiming::new(1, u64::MAX, 1, timer_limits()).is_ok());
    for limits in [
        TimerLimits::new(NonZeroU64::new(400).unwrap(), None),
        TimerLimits::new(NonZeroU64::new(1_000).unwrap(), NonZeroU64::new(400)),
    ] {
        let mut decoder = decoder_with_limits(limits);
        assert_eq!(
            decoder.decode(fixture().ready.as_bytes()).unwrap_err(),
            ProtocolError::TimerRange
        );
        assert!(decoder.ready().is_none());
    }
}
