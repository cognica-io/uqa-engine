//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn diskann_canonical_origin_codec_matches_explicit_fields_and_rejects_malformed_records() {
    let record = DiskANNCanonicalOrigin {
        version: DiskANNVectorVersion::new(
            StorageTransactionId::new(DatabaseId::from_bytes([7; 16]), 9).unwrap(),
            11,
        )
        .unwrap(),
        dimensions: 3,
        count: 2,
    };
    let bytes = record.encode();
    let mut expected = [0; 56];
    expected[..8].copy_from_slice(b"UQAVORG1");
    expected[8..24].fill(7);
    expected[24] = 9;
    expected[32] = 11;
    expected[40] = 3;
    expected[48] = 2;
    assert_eq!(bytes, expected);
    assert_eq!(
        DiskANNCanonicalOrigin::decode(&expected, 3)
            .unwrap()
            .version,
        record.version
    );
    for length in 0..56 {
        assert!(DiskANNCanonicalOrigin::decode(&bytes[..length], 3).is_err());
    }
    assert!(DiskANNCanonicalOrigin::decode(&[0; 57], 3).is_err());
    assert!(DiskANNCanonicalOrigin::decode(&bytes, 4).is_err());
    for (offset, value) in [(0, 0), (24, 0), (32, 0), (44, 1), (53, 1)] {
        let mut malformed = bytes;
        malformed[offset] = value;
        assert!(DiskANNCanonicalOrigin::decode(&malformed, 3).is_err());
    }
    for count in [0, 1, 1_u64 << 32] {
        let record = DiskANNCanonicalOrigin { count, ..record };
        assert_eq!(
            DiskANNCanonicalOrigin::decode(&record.encode(), 3)
                .unwrap()
                .count,
            count
        );
    }
}
