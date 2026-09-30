//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{decode_object_id, metadata_key, SEQUENCE_CATALOG_OID_METADATA_PREFIX};

#[test]
fn object_identities_round_trip_through_metadata_keys() {
    let object_id = [0, 1, 0x7f, 0x80, 0xfe, 0xff, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    let key = metadata_key(&object_id);
    let encoded = key
        .strip_prefix(SEQUENCE_CATALOG_OID_METADATA_PREFIX)
        .unwrap();
    assert_eq!(decode_object_id(encoded).unwrap(), object_id);
    assert!(decode_object_id("00").is_err());
    assert!(decode_object_id(&"zz".repeat(16)).is_err());
}
