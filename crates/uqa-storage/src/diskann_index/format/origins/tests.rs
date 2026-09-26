//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fmt::Write;

use super::*;
use crate::diskann_index::format::DiskANNVectorVersion;
use crate::mvcc::{DatabaseId, StorageTransactionId};

fn entry(document: u64) -> DiskANNOriginEntry {
    let writer = StorageTransactionId::new(DatabaseId::from_bytes([23; 16]), 7).unwrap();
    let version = DiskANNVectorVersion::new(writer, 9).unwrap();
    DiskANNOriginEntry::new(
        document,
        DiskANNCanonicalOrigin::new(version, 2, 0).unwrap(),
    )
}

fn generation() -> DiskANNGeneration {
    DiskANNGeneration::new([11; 16], 1, 2, 3).unwrap()
}

fn reseal(bytes: &mut [u8]) {
    let mut hash = Sha256::new();
    hash.update(&bytes[..64]);
    hash.update(&bytes[96..]);
    bytes[64..96].copy_from_slice(&hash.finalize());
}

#[test]
fn origin_batch_matches_independently_packed_empty_tensor_record() {
    let control = StorageReadControl::with_limit(8192);
    let layout = DiskANNOriginLayout::new(generation(), 2, 1).unwrap();
    let bytes = layout.encode(0, &[entry(42)], &control).unwrap();
    // Python struct.pack('<IIQQQ', 2, 64, 1, 0, 1), a little-endian DocId and the documented canonical origin envelope; hashlib.sha256 covers header[0:64] + body.
    let expected = "555141444e4f520001000000600000000b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0100000000000000020000000000000003000000000000006000000000000000414b9ee0ec165d2c1881809f893218f1438d39e61f412e78850fa9edc21a224d02000000400000000100000000000000000000000000000001000000000000002a00000000000000555141564f524731171717171717171717171717171717170700000000000000090000000000000002000000000000000000000000000000";
    let mut actual = String::new();
    for byte in bytes.iter() {
        write!(&mut actual, "{byte:02x}").unwrap();
    }
    assert_eq!(actual, expected);
    let batch = layout.decode(0, &bytes, &control).unwrap();
    assert_eq!(batch.entry(0), Some(entry(42)));
    assert_eq!(batch.entry(1), None);
    assert_eq!(batch.entry(usize::MAX), None);
    assert_eq!(
        format!("{:x}", Sha256::digest(batch.bytes())),
        "0459b7b642df56be0f84dae6de64ff21042a13ac5644b194d685973808bb2c05"
    );
}

#[test]
fn origin_codec_rejects_rechecksummed_shapes_versions_order_and_foreign_generations() {
    let control = StorageReadControl::with_limit(16 << 10);
    let layout = DiskANNOriginLayout::new(generation(), 2, 1).unwrap();
    let bytes = layout.encode(0, &[entry(42)], &control).unwrap();
    for offset in [8, 96, 100, 104, 112, 120, 136, 160, 168, 176, 180] {
        let mut corrupt = bytes.to_vec();
        corrupt[offset] = if [160, 168].contains(&offset) {
            0
        } else {
            corrupt[offset] ^ 1
        };
        reseal(&mut corrupt);
        assert!(
            layout.decode(0, &corrupt, &control).is_err(),
            "offset {offset}"
        );
    }
    let foreign =
        DiskANNOriginLayout::new(DiskANNGeneration::new([12; 16], 1, 2, 3).unwrap(), 2, 1).unwrap();
    assert!(foreign.decode(0, &bytes, &control).is_err());
    assert!(layout.decode(1, &bytes, &control).is_err());
    assert!(layout
        .decode(0, &bytes[..bytes.len() - 1], &control)
        .is_err());
    let two = DiskANNOriginLayout::new(generation(), 2, 2).unwrap();
    assert!(two.encode(0, &[entry(42), entry(42)], &control).is_err());
    assert!(two.encode(0, &[entry(42), entry(41)], &control).is_err());
    let maximum = DiskANNOriginLayout::new(generation(), 2, u64::MAX).unwrap();
    assert!(maximum.encoded_bytes(u64::MAX).is_err());
    let first = u64::MAX - 63;
    let entries: Vec<_> = (0..63).map(entry).collect();
    let encoded = maximum.encode(first, &entries, &control).unwrap();
    assert_eq!(maximum.decode(first, &encoded, &control).unwrap().len(), 63);
    assert!(DiskANNOriginSummary::new(0, [0; 32]).is_err());
}
