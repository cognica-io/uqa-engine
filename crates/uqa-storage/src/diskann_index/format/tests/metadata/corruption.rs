//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn malformed_manifest_metadata_is_rejected_after_checksum_recalculation() {
    let control = StorageReadControl::with_limit(65_536);
    let bundle = bundle(&control);
    let original = bundle.manifest.encode(&control).unwrap();
    let reader = StorageReadControl::with_limit(0);
    let mut cases: Vec<(usize, Vec<u8>)> = vec![
        (0, b"BADMAGIC".to_vec()),
        (8, 2_u32.to_le_bytes().to_vec()),
        (12, 0_u32.to_le_bytes().to_vec()),
        (16, [1; 16].to_vec()),
        (56, 0_u64.to_le_bytes().to_vec()),
    ];
    for (offset, value) in [
        (0, 0_u32),
        (4, 2),
        (8, 2),
        (12, 2),
        (16, 2),
        (20, 8192),
        (24, 63),
        (28, 145),
    ] {
        cases.push((96 + offset, value.to_le_bytes().to_vec()));
    }
    for (offset, value) in [
        (32, 1_u64),
        (40, 1),
        (48, 0),
        (56, f64::NAN.to_bits()),
        (64, 9),
        (72, 0),
        (88, 9),
        (96, u64::MAX),
        (104, 8),
        (144, 11),
        (152, 2),
    ] {
        cases.push((96 + offset, value.to_le_bytes().to_vec()));
    }
    for (offset, replacement) in cases {
        let mut bytes = original.to_vec();
        bytes[offset..offset + replacement.len()].copy_from_slice(&replacement);
        reseal(&mut bytes);
        assert!(
            DiskANNManifest::decode(generation(), &bytes, &reader).is_err(),
            "offset {offset}"
        );
    }
    for len in 0..original.len() {
        assert!(DiskANNManifest::decode(generation(), &original[..len], &reader).is_err());
    }
    let mut bytes = original.to_vec();
    bytes.push(0);
    bytes[56..64].copy_from_slice(&289_u64.to_le_bytes());
    reseal(&mut bytes);
    assert!(DiskANNManifest::decode(generation(), &bytes, &reader).is_err());
    let mut bytes = original.to_vec();
    bytes[64] ^= 1;
    assert!(DiskANNManifest::decode(generation(), &bytes, &reader).is_err());
    let mut input = *bundle.manifest.input();
    input.coverage.generation = DiskANNGeneration::new([1; 16], 11, 12, 13).unwrap();
    assert!(DiskANNManifest::new(input).is_err());
    assert_eq!(reader.memory().used(), 0);
}

#[test]
fn malformed_codebooks_reject_untrusted_counts_offsets_provenance_and_scalars() {
    let source = StorageReadControl::with_limit(65_536);
    let bundle = bundle(&source);
    let reader = StorageReadControl::with_limit(4096);
    let mut cases: Vec<(usize, Vec<u8>)> = Vec::new();
    for (offset, value) in [
        (0, 0_u32),
        (4, 0),
        (8, 0),
        (8, 257),
        (12, 2),
        (16, 2),
        (20, 2),
        (24, 0),
        (24, 4),
        (28, 0),
        (28, 257),
        (32, 0),
        (32, 257),
        (36, 0),
        (36, 6),
        (64, 1),
        (68, 2),
        (72, 4),
    ] {
        cases.push((offset, value.to_le_bytes().to_vec()));
    }
    for (offset, value) in [
        (40, 43_u64),
        (48, 9),
        (56, 9),
        (76, f64::NAN.to_bits()),
        (76, f64::INFINITY.to_bits()),
        (76, 2.01_f64.to_bits()),
        (76, (-1.0e200_f64).to_bits()),
    ] {
        cases.push((offset, value.to_le_bytes().to_vec()));
    }
    cases.extend([(10, vec![1]), (34, vec![1])]);
    for (offset, replacement) in cases {
        let mut bytes = bundle.book_bytes.to_vec();
        bytes[96 + offset..96 + offset + replacement.len()].copy_from_slice(&replacement);
        reseal(&mut bytes);
        // Rebind the outer digest too, so the inner decoder must reject the actual malformed field.
        let mut input = *bundle.manifest.input();
        input.artifacts.codebook = artifact_digest(&bytes, &reader).unwrap();
        let manifest = DiskANNManifest::new(input).unwrap();
        assert!(
            decode_codebook(&manifest, &bytes, &reader).is_err(),
            "offset {offset}"
        );
        assert_eq!(reader.memory().used(), 0);
    }
    let mut trailing = bundle.book_bytes.to_vec();
    trailing.push(0);
    let body_len = (trailing.len() - 96) as u64;
    trailing[56..64].copy_from_slice(&body_len.to_le_bytes());
    reseal(&mut trailing);
    let mut input = *bundle.manifest.input();
    input.artifacts.codebook = artifact_digest(&trailing, &reader).unwrap();
    assert!(decode_codebook(&DiskANNManifest::new(input).unwrap(), &trailing, &reader).is_err());
    for len in 0..bundle.book_bytes.len() {
        assert!(decode_codebook(&bundle.manifest, &bundle.book_bytes[..len], &reader).is_err());
    }
    let mut bytes = bundle.book_bytes.to_vec();
    bytes[96 + 76..96 + 84].copy_from_slice(&0.25_f64.to_bits().to_le_bytes());
    reseal(&mut bytes);
    assert!(decode_codebook(&bundle.manifest, &bytes, &reader).is_err());
    assert_eq!(reader.memory().used(), 0);
}

#[test]
fn code_batches_cannot_cross_codebooks_or_accept_invalid_labels_and_extents() {
    let control = StorageReadControl::with_limit(65_536);
    let bundle = bundle(&control);
    let other = train(43, &control);
    let (_, other_identity) = encode_codebook(generation(), &other, &control).unwrap();
    assert!(other_identity
        .decode_codes(0, &bundle.code_batch, &control)
        .is_err());
    let other_generation = DiskANNGeneration::new([0x22; 16], 11, 12, 14).unwrap();
    let (_, other_identity) = encode_codebook(other_generation, &bundle.book, &control).unwrap();
    assert!(other_identity
        .decode_codes(0, &bundle.code_batch, &control)
        .is_err());
    let reader = StorageReadControl::with_limit(0);
    for offset in [96, 100, 104, 108, 112, 120, 128, 136, 168] {
        let mut bytes = bundle.code_batch.to_vec();
        bytes[offset] = bytes[offset].wrapping_add(2);
        reseal(&mut bytes);
        assert!(
            bundle.identity.decode_codes(0, &bytes, &reader).is_err(),
            "offset {offset}"
        );
    }
    let mut codes = bundle.codes.clone();
    codes[0] = 2;
    assert!(bundle.identity.encode_codes(0, &codes, &control).is_err());
    assert!(bundle
        .identity
        .encode_codes(0, &bundle.codes[..15], &control)
        .is_err());
    assert!(bundle
        .identity
        .encode_codes(1, &bundle.codes, &control)
        .is_err());
    for len in 0..bundle.code_batch.len() {
        assert!(bundle
            .identity
            .decode_codes(0, &bundle.code_batch[..len], &reader)
            .is_err());
    }
    assert_eq!(reader.memory().used(), 0);
}

#[test]
fn side_batches_reject_wrong_classification_origins_order_and_context() {
    let control = StorageReadControl::with_limit(65_536);
    let bundle = bundle(&control);
    let reader = StorageReadControl::with_limit(0);
    for (offset, replacement) in [
        (96, 0_u32.to_le_bytes().to_vec()),
        (100, 2_u32.to_le_bytes().to_vec()),
        (104, 3_u64.to_le_bytes().to_vec()),
        (112, 1_u64.to_le_bytes().to_vec()),
        (120, 1_u64.to_le_bytes().to_vec()),
        (140, 0_u32.to_le_bytes().to_vec()),
        (140, 3_u32.to_le_bytes().to_vec()),
        (160, 0_u64.to_le_bytes().to_vec()),
        (168, 0_u64.to_le_bytes().to_vec()),
        (128, 201_u64.to_le_bytes().to_vec()),
        (184, 0_u32.to_le_bytes().to_vec()),
    ] {
        let mut bytes = bundle.side_batch.to_vec();
        bytes[offset..offset + replacement.len()].copy_from_slice(&replacement);
        reseal(&mut bytes);
        assert!(
            bundle.side_layout.decode(0, &bytes, &reader).is_err(),
            "offset {offset}"
        );
    }
    for raw in [[1.0; 5], [f32::NAN; 5], [f32::INFINITY; 5]] {
        assert!(DiskANNSideEntry::from_raw(5, 200, 0, version(), &raw, &control).is_err());
    }
    let entry = DiskANNSideEntry::from_raw(3, 200, 0, version(), &[0.0; 3], &control).unwrap();
    assert!(bundle.side_layout.encode(0, &[entry], &control).is_err());
    let entries = entries(&control);
    for invalid in [[entries[0], entries[0]], [entries[1], entries[0]]] {
        assert!(bundle.side_layout.encode(0, &invalid, &control).is_err());
    }
    let different =
        DiskANNSideLayout::new(DiskANNGeneration::new([1; 16], 11, 12, 13).unwrap(), 5, 2).unwrap();
    assert!(different.decode(0, &bundle.side_batch, &reader).is_err());
    for len in 0..bundle.side_batch.len() {
        assert!(bundle
            .side_layout
            .decode(0, &bundle.side_batch[..len], &reader)
            .is_err());
    }
    assert_eq!(reader.memory().used(), 0);
}
