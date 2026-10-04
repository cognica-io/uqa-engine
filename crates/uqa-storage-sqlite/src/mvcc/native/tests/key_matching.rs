//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate canonical key bytes independently of the encoder and without a second key allocation.

use super::*;

#[test]
fn native_key_matching_preserves_signed_integer_extremes_without_allocations() {
    let identity = NativeRecordIdentity::new(NativeRecordFamily::Documents, owner()).unwrap();
    let control = StorageReadControl::with_limit(0);
    for (value, encoded) in [
        (i64::MIN, [0; 8]),
        (-1, [0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
        (0, [0x80, 0, 0, 0, 0, 0, 0, 0]),
        (i64::MAX, [0xff; 8]),
    ] {
        let mut key = b"\0uqa-native-record\x01".to_vec();
        key.extend_from_slice(&identity.family().id().to_be_bytes());
        key.push(1);
        key.extend_from_slice(&[3; 16]);
        key.extend_from_slice(&[7; 16]);
        key.push(1);
        key.extend_from_slice(&encoded);
        let row = [ValueRef::Text(b"docs"), ValueRef::Integer(value)];
        assert!(identity.matches_row_key(&key, &row, &control).unwrap());
        let mut changed = row;
        changed[1] = ValueRef::Integer(value.wrapping_add(1));
        assert!(!identity.matches_row_key(&key, &changed, &control).unwrap());
        key.push(0);
        assert!(!identity.matches_row_key(&key, &row, &control).unwrap());
    }
    assert_eq!(control.memory().peak(), 0);
}

#[test]
fn native_key_matching_checks_zero_escapes_tags_terminators_and_cancellation() {
    let identity =
        NativeRecordIdentity::new(NativeRecordFamily::OccurrenceClusters, owner()).unwrap();
    let mut key = b"\0uqa-native-record\x01".to_vec();
    key.extend_from_slice(&identity.family().id().to_be_bytes());
    key.push(1);
    key.extend_from_slice(&[3; 16]);
    key.extend_from_slice(&[7; 16]);
    let header = key.len();
    key.extend_from_slice(&[2, b'f', 0, 255, b'x', 0, 0, 3, 0, 255, 255, 0, 0, 1]);
    key.extend_from_slice(&[0x80, 0, 0, 0, 0, 0, 0, 9]);
    let row = [
        ValueRef::Text(b"docs"),
        ValueRef::Text(b"f\0x"),
        ValueRef::Blob(b"\0\xff"),
        ValueRef::Integer(9),
    ];
    let control = StorageReadControl::with_limit(0);
    assert!(identity.matches_row_key(&key, &row, &control).unwrap());
    for index in header..key.len() {
        let mut changed = key.clone();
        changed[index] ^= 1;
        assert!(!identity.matches_row_key(&changed, &row, &control).unwrap());
        assert!(!identity
            .matches_row_key(&key[..index], &row, &control)
            .unwrap());
    }
    assert_eq!(control.memory().peak(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        identity
            .matches_row_key(&key, &row, &control)
            .unwrap_err()
            .into_storage_error(),
        uqa_storage::StorageBackendError::Cancelled(_)
    ));
}
