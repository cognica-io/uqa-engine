//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const RECORD: &str = concat!(
    "UQA notification publication 1\n",
    "abababababababababababababababab\n17\n8180\n42\n2\n",
    "6:events3:one7:한:글9:line\n:끝"
);

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
    NotificationPublication::encode([0xab; 16], 17, 8_180, 42, &messages(), control)
}

#[test]
fn publication_matches_independent_framing_and_page_boundary_expectations() {
    let control = StorageReadControl::with_limit(4_096);
    let publication = encode(&control).unwrap();
    assert_eq!(publication.bytes(), RECORD.as_bytes());
    let expected_header = NotificationPublicationHeader {
        registry_id: [0xab; 16],
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
        ("publication 1", "publication 2"),
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
        NotificationPublication::encode([1; 16], 0, 0, 1, &[largest], &control).unwrap();
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
            NotificationPublication::encode([1; 16], 0, 0, 1, &[invalid_message], &control),
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
    let publication =
        NotificationPublication::encode([1; 16], maximum - 1, 0, i32::MAX, &smallest, &control)
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
                registry, sequence, position, process, &smallest, &control
            ),
            Err(VersionError::InvalidEncoding(_))
        ));
        assert_eq!(control.memory().used(), 0);
    }
    assert!(matches!(
        NotificationPublication::encode([1; 16], 0, 0, 1, &[], &control),
        Err(VersionError::InvalidEncoding(_))
    ));
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
