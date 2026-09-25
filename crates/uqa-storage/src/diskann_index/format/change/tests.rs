//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn diskann_change_identity_matches_fixed_bytes_and_document_ranges() {
    let bytes = [
        0, 0, 0, 0, 0, 0, 0, 7, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 17, 1,
        35, 69, 103, 137, 171, 205, 239, 15, 237, 203, 169, 135, 101, 67, 33,
    ];
    let writer =
        StorageTransactionId::new(DatabaseId::from_bytes([17; 16]), 0x0123_4567_89ab_cdef).unwrap();
    let version = DiskANNVectorVersion::new(writer, 0x0fed_cba9_8765_4321).unwrap();
    let identity = DiskANNChangeIdentity::new(7, version);
    assert_eq!(identity.encode(), bytes);
    assert_eq!(DiskANNChangeIdentity::decode(&bytes).unwrap(), identity);
    assert_eq!(identity.document(), 7);
    assert_eq!(identity.version(), version);
    assert!(bytes < DiskANNChangeIdentity::document_end(7));
    assert!(
        DiskANNChangeIdentity::document_end(7) < DiskANNChangeIdentity::new(8, version).encode()
    );
    assert_eq!(DiskANNChangeIdentity::document_end(u64::MAX), [255; 40]);
    for width in 0..40 {
        assert!(DiskANNChangeIdentity::decode(&bytes[..width]).is_err());
    }
    assert!(DiskANNChangeIdentity::decode(&[0; 41]).is_err());
    for start in [24, 32] {
        let mut invalid = bytes;
        invalid[start..start + 8].fill(0);
        assert!(DiskANNChangeIdentity::decode(&invalid).is_err());
    }
}
