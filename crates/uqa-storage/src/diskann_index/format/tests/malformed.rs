//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn reseal(bytes: &mut [u8]) {
    let mut hash = Sha256::new();
    hash.update(&bytes[..112]);
    hash.update(&bytes[144..]);
    bytes[112..144].copy_from_slice(&hash.finalize());
}

#[test]
fn page_checks_reject_rechecksummed_identity_layout_and_fragment_corruption() {
    let control = StorageReadControl::with_limit(16_384);
    let (layout, bytes) = packed(&control);
    let reader = StorageReadControl::with_limit(0);
    for offset in [
        0, 8, 12, 16, 32, 40, 48, 56, 64, 72, 80, 84, 88, 96, 100, 104, 108,
    ] {
        let mut corrupt = bytes.to_vec();
        corrupt[offset] ^= 1;
        reseal(&mut corrupt);
        assert!(
            decode_page(generation(), layout, 0, &corrupt, &reader).is_err(),
            "offset {offset}"
        );
    }
    for offset in [112, 144, 4095] {
        let mut corrupt = bytes.to_vec();
        corrupt[offset] ^= 1;
        assert!(decode_page(generation(), layout, 0, &corrupt, &reader).is_err());
    }
    let mut nonzero_padding = bytes.to_vec();
    nonzero_padding[4095] = 1;
    reseal(&mut nonzero_padding);
    assert!(decode_page(generation(), layout, 0, &nonzero_padding, &reader).is_err());
    let alias = DiskANNNodeLayout::new(1, 3, 3).unwrap();
    assert_eq!(alias.slot_bytes(), layout.slot_bytes());
    assert!(decode_page(generation(), alias, 0, &bytes, &reader).is_err());
    assert_eq!(reader.memory().used(), 0);
}

#[test]
fn truncated_extended_or_misaddressed_pages_and_slots_are_rejected() {
    let control = StorageReadControl::with_limit(16_384);
    let (layout, bytes) = packed(&control);
    let reader = StorageReadControl::with_limit(4096);
    for len in 0..PAGE_BYTES {
        assert!(decode_page(generation(), layout, 0, &bytes[..len], &reader).is_err());
    }
    let mut extended = bytes.to_vec();
    extended.push(0);
    assert!(decode_page(generation(), layout, 0, &extended, &reader).is_err());
    assert!(decode_page(generation(), layout, 1, &bytes, &reader).is_err());
    let node = layout.encode_node(&input(1), &control).unwrap();
    for len in 0..node.len() {
        assert!(layout.decode_node(1, &node[..len], &reader).is_err());
    }
    assert!(layout.decode_node(0, &node, &reader).is_err());
    let mut extended = node.to_vec();
    extended.push(0);
    assert!(layout.decode_node(1, &extended, &reader).is_err());
    assert_eq!(reader.memory().used(), 0);
}

#[test]
fn node_validation_rejects_invalid_norm_origins_degree_and_neighbors() {
    let control = StorageReadControl::with_limit(16_384);
    let layout = DiskANNNodeLayout::new(3, 2, 3).unwrap();
    let node = layout.encode_node(&input(1), &control).unwrap();
    let retained = control.memory().used();
    let cases = [
        (20, 1.0_f32.to_bits().to_le_bytes().to_vec()),
        (24, 3_u64.to_le_bytes().to_vec()),
        (48, 0_u64.to_le_bytes().to_vec()),
        (56, 0_u64.to_le_bytes().to_vec()),
        (64, f32::NAN.to_bits().to_le_bytes().to_vec()),
        (76, 1_u64.to_le_bytes().to_vec()),
        (76, 3_u64.to_le_bytes().to_vec()),
        (84, 0_u64.to_le_bytes().to_vec()),
        (24, 1_u64.to_le_bytes().to_vec()),
    ];
    for (offset, replacement) in cases {
        let mut corrupt = node.to_vec();
        corrupt[offset..offset + replacement.len()].copy_from_slice(&replacement);
        assert!(
            layout.decode_node(1, &corrupt, &control).is_err(),
            "offset {offset}"
        );
        assert_eq!(control.memory().used(), retained);
    }
    for vector in [
        &[0.0, 0.0, 0.0][..],
        &[1.0e-30, 0.0, 0.0],
        &[f32::MAX, 0.0, 0.0],
        &[f32::INFINITY, 0.0, 0.0],
        &[1.0],
    ] {
        let source = DiskANNNodeInput { vector, ..input(1) };
        assert!(layout.encode_node(&source, &control).is_err());
    }
    for neighbors in [&[0, 0][..], &[2, 0], &[1], &[3], &[0, 1, 2]] {
        let source = DiskANNNodeInput {
            neighbors,
            ..input(1)
        };
        assert!(layout.encode_node(&source, &control).is_err());
    }
    assert_eq!(control.memory().used(), retained);
}

#[test]
fn an_envelope_checksum_does_not_replace_complete_node_validation() {
    let control = StorageReadControl::with_limit(16_384);
    let (layout, bytes) = packed(&control);
    let mut corrupt = bytes.to_vec();
    corrupt[PAGE_HEADER_BYTES + 64..PAGE_HEADER_BYTES + 68]
        .copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
    reseal(&mut corrupt);
    let page = decode_page(generation(), layout, 0, &corrupt, &control).unwrap();
    assert!(layout
        .decode_node(0, &page.payload()[..layout.slot_bytes()], &control)
        .is_err());
}
