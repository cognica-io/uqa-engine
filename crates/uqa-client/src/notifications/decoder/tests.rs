//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::notifications::tests::{event, notification_body, ready_decoder};
use serde_json::json;
use uqa_core::notifications::NotificationEvent;

#[test]
fn sequences_above_binary64_and_at_u64_max_remain_exact() {
    for previous in [(1_u64 << 53) - 1, 1_u64 << 53, u64::MAX - 1] {
        let mut decoder = ready_decoder();
        // Establish an otherwise ready epoch's prior counter without replaying quadrillions of frames.
        decoder.sequence = previous;
        let mut body = notification_body();
        body["sequence"] = json!((previous + 1).to_string());
        let step = decoder.decode(&event("notification", &body)).unwrap();
        assert!(
            matches!(step.event, Some(NotificationWireEvent::Notification(NotificationEvent::Notification { sequence, .. })) if sequence == previous + 1)
        );
        assert_eq!(decoder.sequence, previous + 1);
        if previous == u64::MAX - 1 {
            body["sequence"] = json!("1");
            assert_eq!(
                decoder.decode(&event("notification", &body)).unwrap_err(),
                ProtocolError::Sequence
            );
            assert_eq!(decoder.sequence, u64::MAX);
        }
    }
}

#[test]
fn rejected_envelopes_do_not_advance_sequence_or_publish_readiness() {
    let mut decoder = ready_decoder();
    let mut body = notification_body();
    body["channel"] = json!("unsubscribed");
    assert_eq!(
        decoder.decode(&event("notification", &body)).unwrap_err(),
        ProtocolError::InvalidChannels
    );
    assert_eq!(decoder.sequence, 0);
    assert!(decoder.framer.frame().is_empty());
    assert!(decoder.failure.is_some());
}
