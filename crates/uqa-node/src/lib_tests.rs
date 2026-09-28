//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::value::value_from_js_number;

#[test]
fn unsafe_integer_numbers_require_bigint() {
    let error = value_from_js_number((MAX_SAFE_INTEGER + 1) as f64)
        .expect_err("unsafe integer-valued Numbers must not become approximate floats");
    assert!(error.to_string().contains("pass a BigInt"));
    assert_eq!(
        value_from_js_number(MAX_SAFE_INTEGER as f64).unwrap(),
        Value::Int(MAX_SAFE_INTEGER)
    );
}

#[test]
fn fractional_and_non_finite_numbers_remain_floats() {
    assert_eq!(value_from_js_number(1.5).unwrap(), Value::Float(1.5));
    assert!(matches!(
        value_from_js_number(f64::NAN).unwrap(),
        Value::Float(value) if value.is_nan()
    ));
}

#[test]
fn notification_values_preserve_full_width_and_disjoint_variants() {
    use crate::notifications::NativeNotificationEvent;
    use uqa_core::notifications::{
        NotificationEpoch, NotificationEvent, NotificationFailureKind, NotificationIdentity,
        SQLNotification,
    };

    let identity = NotificationIdentity {
        epoch: "919108f7-52d1-4320-9bac-f847db4148a8"
            .parse::<NotificationEpoch>()
            .unwrap(),
        request_id: None,
    };
    let native = NativeNotificationEvent::from(NotificationEvent::Notification {
        identity: identity.clone(),
        sequence: u64::MAX,
        notification: SQLNotification {
            process_id: i32::MIN,
            channel: "작업".into(),
            payload: "{\"😀\":\"값\"}\n".into(),
        },
    });
    let sequence = native.sequence.unwrap();
    assert!(!sequence.sign_bit);
    assert_eq!(sequence.words, [u64::MAX]);
    assert_eq!(native.process_id, Some(i32::MIN));
    assert_eq!(native.channel.as_deref(), Some("작업"));
    assert_eq!(native.payload.as_deref(), Some("{\"😀\":\"값\"}\n"));
    assert_eq!(native.epoch, identity.epoch.to_string());
    assert!(native.request_id.is_none());
    assert!(native.cause.is_none());

    let gap = NativeNotificationEvent::from(NotificationEvent::ResyncRequired {
        identity: identity.clone(),
        cause: NotificationFailureKind::SourceUnavailable,
    });
    let reconnected = NativeNotificationEvent::from(NotificationEvent::Reconnected { identity });
    assert_eq!(gap.kind, "resync_required");
    assert_eq!(
        gap.cause.as_deref(),
        Some("NOTIFICATION_SOURCE_UNAVAILABLE")
    );
    assert_eq!(reconnected.kind, "reconnected");
    assert!(reconnected.cause.is_none());
    for event in [gap, reconnected] {
        assert!(event.sequence.is_none());
        assert!(event.process_id.is_none());
        assert!(event.channel.is_none());
        assert!(event.payload.is_none());
    }
}
