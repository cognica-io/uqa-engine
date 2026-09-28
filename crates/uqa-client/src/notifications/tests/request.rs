//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn parse(input: &str) -> Result<SubscriptionRequest, ProtocolError> {
    SubscriptionRequest::from_json(input.as_bytes(), maximum_channels(), None)
}

#[test]
fn exact_channels_form_an_order_independent_set_without_normalization() {
    let channels = ["jobs", "Jobs", "é", "e\u{301}", "quote\"; --", "작업"];
    let request = SubscriptionRequest::new(&channels, maximum_channels()).unwrap();
    for channel in channels {
        assert!(request.contains(channel));
    }
    assert!(!request.contains("JOBS"));
    let mut reversed = channels;
    reversed.reverse();
    assert_eq!(
        request,
        SubscriptionRequest::new(&reversed, maximum_channels()).unwrap()
    );
    assert_eq!(
        request,
        SubscriptionRequest::from_json(&request.encode().unwrap(), maximum_channels(), None)
            .unwrap()
    );
    assert_eq!(parse(&fixture().valid_request).unwrap().channels().len(), 2);
}

#[test]
fn request_validates_all_fields_and_never_accepts_resume() {
    for input in [
        "{}",
        "[]",
        "null",
        "{\"channels\":[\"jobs\"]}",
        "{\"protocol_version\":1}",
        "{\"protocol_version\":1.0,\"channels\":[\"jobs\"]}",
        "{\"protocol_version\":true,\"channels\":[\"jobs\"]}",
        "{\"protocol_version\":1,\"channels\":[\"jobs\"],\"resume\":null}",
        "{\"protocol_version\":1,\"protocol_version\":1,\"channels\":[\"jobs\"]}",
        "{\"protocol_version\":1,\"channels\":[\"jobs\"],\"chann\\u0065ls\":[\"jobs\"]}",
        "{\"protocol_version\":1,\"channels\":[true]}",
        "{\"protocol_version\":1,\"channels\":\"jobs\"}",
    ] {
        assert!(parse(input).is_err(), "accepted {input}");
    }
    for version in [0, 2, u64::MAX] {
        assert_eq!(
            parse(&format!(
                r#"{{"protocol_version":{version},"channels":["jobs"]}}"#
            )),
            Err(ProtocolError::UnsupportedVersion)
        );
    }
    let fixture = fixture();
    for header in [b"cursor".as_slice(), b" ", b"\0"] {
        assert_eq!(
            SubscriptionRequest::from_json(
                fixture.valid_request.as_bytes(),
                maximum_channels(),
                Some(header)
            ),
            Err(ProtocolError::ResumeUnsupported)
        );
    }
    assert!(SubscriptionRequest::from_json(
        fixture.valid_request.as_bytes(),
        maximum_channels(),
        Some(b"")
    )
    .is_ok());
}

#[test]
fn channel_limits_use_exact_utf8_bytes_and_reject_duplicates() {
    for channel in [
        String::new(),
        "x".repeat(64),
        "한".repeat(22),
        "nul\0channel".into(),
    ] {
        assert_eq!(
            SubscriptionRequest::new(&[&channel], maximum_channels()),
            Err(ProtocolError::InvalidChannels)
        );
        assert_eq!(
            parse(&serde_json::json!({"protocol_version":1,"channels":[channel]}).to_string()),
            Err(ProtocolError::InvalidChannels)
        );
    }
    for channel in ["x".repeat(63), "한".repeat(21)] {
        assert!(SubscriptionRequest::new(&[&channel], maximum_channels()).is_ok());
    }
    assert_eq!(
        SubscriptionRequest::new(&[], maximum_channels()),
        Err(ProtocolError::InvalidChannels)
    );
    assert_eq!(
        parse(r#"{"protocol_version":1,"channels":[]}"#),
        Err(ProtocolError::InvalidChannels)
    );
    assert_eq!(
        parse(r#"{"protocol_version":1,"channels":["jobs","j\u006fbs"]}"#),
        Err(ProtocolError::InvalidChannels)
    );
    assert_eq!(
        SubscriptionRequest::new(&["a", "a"], maximum_channels()),
        Err(ProtocolError::InvalidChannels)
    );
    let one = NonZeroUsize::new(1).unwrap();
    assert_eq!(
        SubscriptionRequest::new(&["a", "b"], one),
        Err(ProtocolError::ChannelLimit)
    );
    assert_eq!(
        SubscriptionRequest::from_json(fixture().valid_request.as_bytes(), one, None),
        Err(ProtocolError::ChannelLimit)
    );
}

#[test]
fn request_byte_limit_counts_raw_input_and_escaped_output() {
    let fixture = fixture();
    let mut input = fixture.valid_request.into_bytes();
    input.resize(MAX_NOTIFICATION_WIRE_BYTES, b' ');
    assert!(SubscriptionRequest::from_json(&input, maximum_channels(), None).is_ok());
    input.push(b' ');
    assert_eq!(
        SubscriptionRequest::from_json(&input, maximum_channels(), None),
        Err(ProtocolError::ByteLimit)
    );
    let channels: Vec<_> = (0..200)
        .map(|index| format!("{index:03}{}", "\u{1}".repeat(60)))
        .collect();
    let borrowed: Vec<_> = channels.iter().map(String::as_str).collect();
    // Decoded channel bytes fit, but six-byte JSON escapes must be counted before making owned channel copies.
    assert!(channels.iter().map(String::len).sum::<usize>() < MAX_NOTIFICATION_WIRE_BYTES);
    assert_eq!(
        SubscriptionRequest::new(&borrowed, maximum_channels()),
        Err(ProtocolError::ByteLimit)
    );
}

#[test]
fn nesting_and_grammar_are_checked_before_schema_materialization() {
    assert_eq!(
        parse(&fixture().invalid_nested_request),
        Err(ProtocolError::InvalidJSON)
    );
    for input in [
        r#"{"protocol_version":1,"channels":["jobs"],"extra":{"a":{}}}"#,
        r#"{"protocol_version":1,"channels":["\uD800"]}"#,
        r#"{"protocol_version":1,"channels":["jobs",]}"#,
        r#"{"protocol_version":1,"channels":["jobs"]}{}"#,
    ] {
        assert_eq!(parse(input), Err(ProtocolError::InvalidJSON));
    }
    let deep = format!(
        "{{\"unexpected\":{}0{}}}",
        "[".repeat(30_000),
        "]".repeat(30_000)
    );
    assert_eq!(parse(&deep), Err(ProtocolError::InvalidJSON));
    let mut invalid = fixture().valid_request.into_bytes();
    invalid.insert(3, 0xff);
    assert_eq!(
        SubscriptionRequest::from_json(&invalid, maximum_channels(), None),
        Err(ProtocolError::InvalidUTF8)
    );
}
