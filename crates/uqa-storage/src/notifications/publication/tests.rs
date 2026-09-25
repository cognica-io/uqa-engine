//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const RECORD: &str = concat!(
    "UQA notification publication 2\n",
    "abababababababababababababababab\n11\n17\n8180\n42\n2\n",
    "6:events3:one7:한:글9:line\n:끝\n0\n"
);

fn start(
    registry_id: [u8; 16],
    first_sequence: u64,
    first_position: u64,
    process_id: i32,
) -> NotificationPublicationStart {
    NotificationPublicationStart {
        registry_id,
        publication_sequence: 11,
        first_sequence,
        first_position,
        process_id,
    }
}

fn messages() -> [PendingNotification; 2] {
    [
        PendingNotification {
            channel: "events".into(),
            payload: "one".into(),
        },
        PendingNotification {
            channel: "한:글".into(),
            payload: "line\n:끝".into(),
        },
    ]
}

fn encode(control: &StorageReadControl) -> VersionResult<NotificationPublication> {
    NotificationPublication::encode(start([0xab; 16], 17, 8_180, 42), &messages(), None, control)
}

#[test]
fn publication_matches_independent_framing_and_page_boundary_expectations() {
    let control = StorageReadControl::with_limit(4_096);
    let publication = encode(&control).unwrap();
    assert_eq!(publication.bytes(), RECORD.as_bytes());
    let expected_header = NotificationPublicationHeader {
        registry_id: [0xab; 16],
        publication_sequence: 11,
        first_sequence: 17,
        next_sequence: 19,
        first_position: 8_180,
        end_position: 8_256,
        process_id: 42,
    };
    assert_eq!(publication.header(), expected_header);
    let borrowed_control = StorageReadControl::with_limit(0);
    let decoded =
        NotificationPublicationView::decode(RECORD.as_bytes(), &borrowed_control).unwrap();
    assert_eq!(decoded.header(), expected_header);
    assert_eq!(decoded.fingerprint(), publication.fingerprint());
    let decoded_messages = decoded
        .messages()
        .collect::<VersionResult<Vec<_>>>()
        .unwrap();
    assert_eq!(
        decoded_messages,
        vec![
            NotificationMessageRef {
                channel: "events",
                payload: "one"
            },
            NotificationMessageRef {
                channel: "한:글",
                payload: "line\n:끝"
            },
        ]
    );
    let channel_offset = RECORD.find("events").unwrap();
    assert_eq!(
        decoded_messages[0].channel.as_ptr(),
        RECORD[channel_offset..].as_ptr()
    );
    assert_eq!(borrowed_control.memory().used(), 0);
}

#[test]
fn publication_without_receivers_preserves_identity_without_advancing_the_queue() {
    let control = StorageReadControl::with_limit(4_096);
    let first =
        NotificationPublication::encode(start([0xab; 16], 17, 8_180, 42), &[], None, &control)
            .unwrap();
    let expected = concat!(
        "UQA notification publication 2\n",
        "abababababababababababababababab\n11\n17\n8180\n42\n0\n\n0\n"
    );
    assert_eq!(first.bytes(), expected.as_bytes());
    let decoded = NotificationPublicationView::decode(
        expected.as_bytes(),
        &StorageReadControl::with_limit(0),
    )
    .unwrap();
    assert_eq!(decoded.header().first_sequence, 17);
    assert_eq!(decoded.header().next_sequence, 17);
    assert_eq!(decoded.header().end_position, 8_180);
    assert!(decoded.messages().next().is_none());
    assert!(decoded.subscription().is_none());
    let mut later = start([0xab; 16], 17, 8_180, 42);
    later.publication_sequence = 12;
    let later = NotificationPublication::encode(later, &[], None, &control).unwrap();
    assert_ne!(first.fingerprint(), later.fingerprint());
}

#[test]
fn every_truncated_record_and_trailing_data_are_rejected() {
    let control = StorageReadControl::with_limit(0);
    for end in 0..RECORD.len() {
        assert!(
            matches!(
                NotificationPublicationView::decode(&RECORD.as_bytes()[..end], &control),
                Err(VersionError::InvalidEncoding(_))
            ),
            "prefix {end}"
        );
    }
    let mut trailing = RECORD.as_bytes().to_vec();
    trailing.push(0);
    assert!(matches!(
        NotificationPublicationView::decode(&trailing, &control),
        Err(VersionError::InvalidEncoding(_))
    ));
    let mut invalid_utf8 = RECORD.as_bytes().to_vec();
    invalid_utf8[RECORD.find("events").unwrap()] = 0xff;
    assert!(matches!(
        NotificationPublicationView::decode(&invalid_utf8, &control),
        Err(VersionError::InvalidEncoding(_))
    ));
}

#[test]
fn malformed_header_lengths_and_counts_fail_without_allocation() {
    let control = StorageReadControl::with_limit(0);
    let changes = [
        ("publication 2", "publication 1"),
        (
            "abababababababababababababababab",
            "00000000000000000000000000000000",
        ),
        (
            "abababababababababababababababab",
            "ABABABABABABABABABABABABABABABAB",
        ),
        ("\n17\n", "\n017\n"),
        ("\n17\n", "\n9223372036854775807\n"),
        ("\n17\n", "\n18446744073709551616\n"),
        ("\n8180\n", "\n-1\n"),
        ("\n8180\n", "\n9223372036854775807\n"),
        ("\n42\n", "\n0\n"),
        ("\n42\n", "\n2147483648\n"),
        ("\n2\n", "\n0\n"),
        ("\n2\n", "\n1\n"),
        ("\n2\n", "\n3\n"),
        ("6:events", "06:events"),
        ("6:events", "64:events"),
        ("6:events", "0:"),
        ("3:one", "8000:one"),
        ("3:one", "18446744073709551616:one"),
    ];
    for (from, to) in changes {
        let record = RECORD.replacen(from, to, 1);
        assert!(
            matches!(
                NotificationPublicationView::decode(record.as_bytes(), &control),
                Err(VersionError::InvalidEncoding(_))
            ),
            "{from:?} -> {to:?}"
        );
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn field_limits_count_utf8_bytes_and_allow_the_largest_valid_payload() {
    let control = StorageReadControl::with_limit(65_536);
    let largest = PendingNotification {
        channel: "c".repeat(63),
        payload: "p".repeat(7_999),
    };
    let publication =
        NotificationPublication::encode(start([1; 16], 0, 0, 1), &[largest], None, &control)
            .unwrap();
    assert_eq!(publication.header().end_position, 8_080);
    let item = publication.view().messages().next().unwrap().unwrap();
    assert_eq!(item.channel.len(), 63);
    assert_eq!(item.payload.len(), 7_999);
    drop(publication);
    for invalid_message in [
        PendingNotification {
            channel: String::new(),
            payload: String::new(),
        },
        PendingNotification {
            channel: "c".repeat(64),
            payload: String::new(),
        },
        PendingNotification {
            channel: "한".repeat(22),
            payload: String::new(),
        },
        PendingNotification {
            channel: "c".into(),
            payload: "p".repeat(8_000),
        },
    ] {
        assert!(matches!(
            NotificationPublication::encode(
                start([1; 16], 0, 0, 1),
                &[invalid_message],
                None,
                &control
            ),
            Err(VersionError::InvalidEncoding(_))
        ));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn identities_and_position_arithmetic_cannot_wrap() {
    let control = StorageReadControl::with_limit(4_096);
    let smallest = [PendingNotification {
        channel: "c".into(),
        payload: String::new(),
    }];
    let maximum = i64::MAX as u64;
    let publication = NotificationPublication::encode(
        start([1; 16], maximum - 1, 0, i32::MAX),
        &smallest,
        None,
        &control,
    )
    .unwrap();
    assert_eq!(publication.header().next_sequence, maximum);
    drop(publication);
    for (registry, sequence, position, process) in [
        ([0; 16], 0, 0, 1),
        ([1; 16], maximum, 0, 1),
        ([1; 16], u64::MAX, 0, 1),
        ([1; 16], 0, maximum, 1),
        ([1; 16], 0, u64::MAX, 1),
        ([1; 16], 0, 0, 0),
        ([1; 16], 0, 0, -1),
    ] {
        assert!(matches!(
            NotificationPublication::encode(
                start(registry, sequence, position, process),
                &smallest,
                None,
                &control
            ),
            Err(VersionError::InvalidEncoding(_))
        ));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn immutable_clones_retain_one_original_admission_until_final_drop() {
    let control = StorageReadControl::with_limit(4_096);
    let publication = encode(&control).unwrap();
    let charged = control.memory().used();
    assert!(charged >= RECORD.len());
    let retained = publication.clone();
    assert_eq!(retained.bytes().as_ptr(), publication.bytes().as_ptr());
    assert_eq!(control.memory().used(), charged);
    drop(publication);
    assert_eq!(control.memory().used(), charged);
    assert_eq!(retained.view().messages().count(), 2);
    drop(retained);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn quota_exhaustion_and_cancellation_release_partial_admission() {
    for limit in [0, MAGIC.len(), RECORD.len() - 1] {
        let control = StorageReadControl::with_limit(limit);
        assert!(matches!(encode(&control), Err(VersionError::Memory(_))));
        assert_eq!(control.memory().used(), 0);
    }
    let control = StorageReadControl::with_limit(4_096);
    control.cancellation().cancel();
    assert!(matches!(encode(&control), Err(VersionError::Cancelled(_))));
    assert!(matches!(
        NotificationPublicationView::decode(RECORD.as_bytes(), &control),
        Err(VersionError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn publication_fingerprint_distinguishes_payloads_with_the_same_identity() {
    let control = StorageReadControl::with_limit(4_096);
    let original = encode(&control).unwrap();
    let changed = RECORD.replace("3:one", "3:two");
    let changed = NotificationPublicationView::decode(changed.as_bytes(), &control).unwrap();
    assert_eq!(original.header(), changed.header());
    assert_ne!(original.fingerprint(), changed.fingerprint());
}

fn listener(channels: Vec<String>) -> NotificationListenerRow {
    NotificationListenerRow {
        owner_id: [0xcd; 16],
        session_id: u64::MAX,
        process_id: 42,
        wake_port: 1234,
        channels,
        transaction_open: false,
        next_sequence: 9,
        position: 40,
    }
}

#[test]
fn subscription_only_publications_borrow_listen_and_unlisten_without_advancing_messages() {
    for channels in [vec!["events".into(), "한글".into()], Vec::new()] {
        let listener = listener(channels.clone());
        let control = StorageReadControl::with_limit(4096);
        let publication = NotificationPublication::encode(
            start([0xab; 16], 17, 8180, 42),
            &[],
            Some(&listener),
            &control,
        )
        .unwrap();
        let suffix = if channels.is_empty() {
            "0\n"
        } else {
            "2\n6:events6:한글"
        };
        let expected = format!(
            "UQA notification publication 2\nabababababababababababababababab\n11\n17\n8180\n42\n0\n\n1\ncdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd\n18446744073709551615\n1234\n9\n40\n{suffix}"
        );
        assert_eq!(publication.bytes(), expected.as_bytes());
        assert_eq!(
            publication.header().first_sequence,
            publication.header().next_sequence
        );
        assert_eq!(
            publication.header().first_position,
            publication.header().end_position
        );
        let borrowed = StorageReadControl::with_limit(0);
        let view = NotificationPublicationView::decode(publication.bytes(), &borrowed).unwrap();
        assert_eq!(view.messages().count(), 0);
        let subscription = view.subscription().unwrap();
        assert_eq!(subscription.owner_id, listener.owner_id);
        assert_eq!(subscription.session_id, listener.session_id);
        assert_eq!(subscription.wake_port, listener.wake_port);
        assert_eq!(subscription.next_sequence, listener.next_sequence);
        assert_eq!(subscription.position, listener.position);
        assert_eq!(
            subscription
                .channels()
                .collect::<VersionResult<Vec<_>>>()
                .unwrap(),
            channels
        );
        assert!(matches!(
            subscription.channels_json(&borrowed),
            Err(VersionError::Memory(_))
        ));
        assert_eq!(borrowed.memory().used(), 0);
        let json_control = StorageReadControl::with_limit(4096);
        let json = subscription.channels_json(&json_control).unwrap();
        let expected_json = if channels.is_empty() {
            "[]"
        } else {
            r#"["events","한글"]"#
        };
        assert_eq!(json.as_ref(), expected_json.as_bytes());
        drop(json);
        assert_eq!(json_control.memory().used(), 0);
        let mut later = start([0xab; 16], 17, 8180, 42);
        later.publication_sequence += 1;
        let later = NotificationPublication::encode(later, &[], Some(&listener), &control).unwrap();
        assert_ne!(later.fingerprint(), publication.fingerprint());
    }
}

#[test]
fn subscription_framing_rejects_truncation_malformed_fields_and_exhausted_identity() {
    let control = StorageReadControl::with_limit(4096);
    let publication = NotificationPublication::encode(
        start([0xab; 16], 17, 8180, 42),
        &messages(),
        Some(&listener(vec!["events".into()])),
        &control,
    )
    .unwrap();
    let borrowed = StorageReadControl::with_limit(0);
    for end in 0..publication.bytes().len() {
        assert!(
            NotificationPublicationView::decode(&publication.bytes()[..end], &borrowed).is_err(),
            "prefix {end}"
        );
    }
    let record = std::str::from_utf8(publication.bytes()).unwrap();
    for (from, to) in [
        ("\n11\n", "\n9223372036854775807\n"),
        ("\n1\ncd", "\n2\ncd"),
        (
            "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
            "00000000000000000000000000000000",
        ),
        ("\n1234\n", "\n0\n"),
        ("\n1234\n", "\n65536\n"),
        ("\n9\n", "\n9223372036854775808\n"),
        ("\n40\n", "\n9223372036854775808\n"),
        ("\n40\n1\n", "\n40\n2\n"),
        ("\n40\n1\n6:events", "\n40\n1\n0:"),
    ] {
        let malformed = record.replacen(from, to, 1);
        assert_ne!(record, malformed);
        assert!(
            NotificationPublicationView::decode(malformed.as_bytes(), &borrowed).is_err(),
            "{from:?} -> {to:?}"
        );
    }
    for changed in [
        NotificationListenerRow {
            owner_id: [0; 16],
            ..listener(Vec::new())
        },
        NotificationListenerRow {
            process_id: 43,
            ..listener(Vec::new())
        },
        NotificationListenerRow {
            transaction_open: true,
            ..listener(Vec::new())
        },
        NotificationListenerRow {
            wake_port: 0,
            ..listener(Vec::new())
        },
        listener(vec![String::new()]),
        listener(vec!["c".repeat(64)]),
    ] {
        assert!(NotificationPublication::encode(
            start([0xab; 16], 17, 8180, 42),
            &[],
            Some(&changed),
            &control
        )
        .is_err());
    }
    assert_eq!(borrowed.memory().used(), 0);
}
