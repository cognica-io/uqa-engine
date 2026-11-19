//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn bytes(header: MemoryImageHeader) -> Vec<u8> {
    let control = StorageReadControl::with_limit(4096);
    let mut reader = header.image_reader(&control).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 3];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    drop(reader);
    assert_eq!(control.memory().used(), 0);
    bytes
}

#[test]
fn memory_header_preserves_absent_zero_and_exhausted_allocator_positions() {
    let control = StorageReadControl::with_limit(4096);
    for position in [None, Some(0), Some(16384), Some(u64::MAX)] {
        let header = MemoryImageHeader {
            source: DatabaseId::from_bytes([17; 16]),
            catalog_oid_position: position,
        };
        let encoded = bytes(header);
        assert_eq!(&encoded[..8], b"UQAMEM01");
        assert_eq!(encoded.len(), 33);
        assert_eq!(
            MemoryImageHeader::read_image(&mut encoded.as_slice(), &control).unwrap(),
            header
        );
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_header_rejects_invalid_tags_hidden_absent_payloads_and_trailing_bytes() {
    let control = StorageReadControl::with_limit(4096);
    let header = MemoryImageHeader {
        source: DatabaseId::from_bytes([17; 16]),
        catalog_oid_position: None,
    };
    let encoded = bytes(header);
    for length in 0..encoded.len() {
        assert!(MemoryImageHeader::read_image(&mut &encoded[..length], &control).is_err());
    }
    for (index, value) in [(0, 0), (24, 2), (25, 1)] {
        let mut bad = encoded.clone();
        bad[index] = value;
        assert!(MemoryImageHeader::read_image(&mut bad.as_slice(), &control).is_err());
    }
    let mut bad = encoded.clone();
    bad[8..24].fill(0);
    assert!(MemoryImageHeader::read_image(&mut bad.as_slice(), &control).is_err());
    let mut bad = encoded;
    bad.push(0);
    assert!(MemoryImageHeader::read_image(&mut bad.as_slice(), &control).is_err());
    assert!(MemoryImageHeader {
        source: DatabaseId::from_bytes([0; 16]),
        ..header
    }
    .image_reader(&control)
    .is_err());
    assert_eq!(control.memory().used(), 0);
}
