//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const EPOCH: &str = "7fb52b7f-bdca-4db2-9ee0-490f99857201";

#[test]
fn epoch_matches_rfc_9562_uuid_v4_reference_vector() {
    // RFC 9562, Appendix A.3, Figure 20; the expected bytes are independent of this codec.
    let bytes = [
        0x91, 0x91, 0x08, 0xf7, 0x52, 0xd1, 0x43, 0x20, 0x9b, 0xac, 0xf8, 0x47, 0xdb, 0x41, 0x48,
        0xa8,
    ];
    let text = "919108f7-52d1-4320-9bac-f847db4148a8";
    assert_eq!(
        NotificationEpoch::from_bytes(bytes).unwrap().to_string(),
        text
    );
    assert_eq!(
        text.parse::<NotificationEpoch>().unwrap().as_bytes(),
        &bytes
    );
}

#[test]
fn epochs_reject_noncanonical_and_wrong_uuid_variants() {
    for invalid in [
        "",
        "7fb52b7fbdca4db29ee0490f99857201",
        "7FB52B7F-BDCA-4DB2-9EE0-490F99857201",
        "7fb52b7f-bdca-5db2-9ee0-490f99857201",
        "7fb52b7f-bdca-4db2-7ee0-490f99857201",
        "7fb52b7f-bdca-4db2-cee0-490f99857201",
        "7fb52b7f-bdca-4db2-9ee0-490f9985720g",
        "7fb52b7f-bdca-4db2-9ee0-490f99857201\n",
        "7fb52b7f-bdca-4db2-9ee0-490f998572é",
        "7fb52b7f_bdca-4db2-9ee0-490f99857201",
    ] {
        assert_eq!(
            invalid.parse::<NotificationEpoch>(),
            Err(InvalidNotificationIdentity::Epoch)
        );
    }
    assert_eq!(
        NotificationEpoch::from_bytes([0; 16]),
        Err(InvalidNotificationIdentity::Epoch)
    );
    let epoch = EPOCH.parse::<NotificationEpoch>().unwrap();
    assert_eq!(
        epoch.as_bytes(),
        &[
            0x7f, 0xb5, 0x2b, 0x7f, 0xbd, 0xca, 0x4d, 0xb2, 0x9e, 0xe0, 0x49, 0x0f, 0x99, 0x85,
            0x72, 0x01
        ]
    );
    assert_eq!(epoch.to_string(), EPOCH);
}

#[test]
fn request_id_validation_is_bounded_and_content_free() {
    let longest = "a".repeat(128);
    assert_eq!(
        NotificationRequestId::new(&longest).unwrap().as_str(),
        longest
    );
    assert_eq!(
        NotificationRequestId::new("Ab_09-z").unwrap().as_str(),
        "Ab_09-z"
    );
    for invalid in ["", "private channel", "secret\r\n", "한", &"a".repeat(129)] {
        let error = NotificationRequestId::new(invalid).unwrap_err();
        assert_eq!(error, InvalidNotificationIdentity::RequestId);
        assert!(!format!("{error:?}: {error}").contains("secret"));
    }
}

#[test]
fn event_values_preserve_exact_integers_and_opaque_text_but_redact_debug() {
    let notification = SQLNotification {
        process_id: i32::MIN,
        channel: "private-한글".into(),
        payload: "opaque\n{\"secret\":\"値\"}".into(),
    };
    for sequence in [1, (1_u64 << 53) + 1, u64::MAX] {
        let event = NotificationEvent::Notification {
            identity: NotificationIdentity {
                epoch: EPOCH.parse().unwrap(),
                request_id: None,
            },
            sequence,
            notification: notification.clone(),
        };
        let debug = format!("{event:?}");
        assert!(debug.contains(EPOCH));
        assert!(debug.contains(&sequence.to_string()));
        assert!(!debug.contains("private"));
        assert!(!debug.contains("secret"));
        let NotificationEvent::Notification {
            identity,
            sequence: actual,
            notification: value,
        } = event
        else {
            panic!("expected notification value");
        };
        assert_eq!(actual, sequence);
        assert_eq!(value, notification);
        assert!(identity.request_id.is_none());
    }
}

proptest::proptest! {
    #[test]
    fn every_uuid_v4_bit_pattern_has_one_lossless_canonical_text(mut bytes in proptest::array::uniform16(proptest::num::u8::ANY)) {
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let epoch = NotificationEpoch::from_bytes(bytes).unwrap();
        let text = epoch.to_string();
        let parsed = text.parse::<NotificationEpoch>().unwrap();
        proptest::prop_assert_eq!(text.len(), 36);
        proptest::prop_assert_eq!(parsed.as_bytes(), &bytes);
    }
}
