//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde_json::Value;
use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

use super::*;
use crate::mvcc::{DatabaseId, StorageTransactionId};

mod malformed;
mod resources;

fn generation() -> DiskANNGeneration {
    DiskANNGeneration::new([0x22; 16], 11, 12, 13).unwrap()
}

fn version() -> DiskANNVectorVersion {
    DiskANNVectorVersion::new(
        StorageTransactionId::new(DatabaseId::from_bytes([0x11; 16]), 7).unwrap(),
        9,
    )
    .unwrap()
}

fn input(node_id: u64) -> DiskANNNodeInput<'static> {
    DiskANNNodeInput {
        node_id,
        doc_id: 41 + node_id,
        ordinal: if node_id == 1 { 3 } else { 0 },
        version: version(),
        vector: &[-0.0, 3.0, 4.0],
        neighbors: match node_id {
            0 => &[1, 2],
            1 => &[0, 2],
            2 => &[0, 1],
            _ => panic!("fixture node"),
        },
    }
}

fn packed(control: &StorageReadControl) -> (DiskANNNodeLayout, BudgetedVec<u8>) {
    let layout = DiskANNNodeLayout::new(3, 2, 3).unwrap();
    let mut payload = BudgetedVec::new(control.memory());
    for id in 0..3 {
        payload
            .extend_from_slice(&layout.encode_node(&input(id), control).unwrap())
            .unwrap();
    }
    let page = encode_page(generation(), layout, 0, &payload, control).unwrap();
    (layout, page)
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../tests/fixtures/diskann/pages.json")).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut result = String::new();
    for byte in bytes {
        write!(result, "{byte:02x}").unwrap();
    }
    result
}

#[test]
fn packed_nodes_and_pages_match_independent_little_endian_bytes() {
    let control = StorageReadControl::with_limit(16_384);
    let (layout, bytes) = packed(&control);
    let fixture = fixture();
    assert_eq!(layout.slot_bytes(), 92);
    assert_eq!(
        hex(&bytes[..PAGE_HEADER_BYTES]),
        fixture["packed_page_header_hex"].as_str().unwrap()
    );
    assert_eq!(
        hex(&Sha256::digest(&bytes[..])),
        fixture["packed_page_sha256"].as_str().unwrap()
    );
    let reader = StorageReadControl::with_limit(0);
    let page = decode_page(generation(), layout, 0, &bytes, &reader).unwrap();
    assert_eq!(page.id(), 0);
    assert_eq!(page.shape().slots, 3);
    for id in 0..3 {
        let address = layout.node_address(id).unwrap();
        let start = address.slot as usize * layout.slot_bytes();
        let raw = &page.payload()[start..start + layout.slot_bytes()];
        if id == 1 {
            assert_eq!(hex(raw), fixture["packed_node_hex"].as_str().unwrap());
        }
        let node = layout.decode_node(id, raw, &control).unwrap();
        let expected = input(id);
        assert_eq!(
            (node.node_id(), node.doc_id(), node.ordinal()),
            (id, expected.doc_id, expected.ordinal)
        );
        assert_eq!(node.version(), version());
        assert_eq!(node.raw_norm().to_bits(), 0x40a0_0000);
        assert_eq!(
            node.vector()
                .iter()
                .map(|x| x.to_bits())
                .collect::<Vec<_>>(),
            [0x8000_0000, 0x4040_0000, 0x4080_0000]
        );
        assert_eq!(node.neighbors(), expected.neighbors);
    }
    assert_eq!(reader.memory().used(), 0);
    drop(bytes);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn a_high_dimensional_node_round_trips_through_every_fragment() {
    let control = StorageReadControl::with_limit(65_536);
    let layout = DiskANNNodeLayout::new(1024, 2, 1).unwrap();
    let mut vector = vec![0.0; 1024];
    vector[0] = 1.0;
    vector[1023] = -0.0;
    let source = DiskANNNodeInput {
        node_id: 0,
        doc_id: 41,
        ordinal: 0,
        version: version(),
        vector: &vector,
        neighbors: &[],
    };
    let encoded = layout.encode_node(&source, &control).unwrap();
    let fixture = fixture();
    assert_eq!(
        encoded.len(),
        fixture["large_node_bytes"].as_u64().unwrap() as usize
    );
    assert_eq!(layout.page_count(), 2);
    assert_eq!(layout.node_address(0).unwrap().fragments, 2);
    let mut assembled = BudgetedVec::new(control.memory());
    assembled.reserve(layout.slot_bytes()).unwrap();
    for (index, payload) in encoded.chunks(PAGE_PAYLOAD_BYTES).enumerate() {
        let bytes = encode_page(generation(), layout, index as u64, payload, &control).unwrap();
        assert_eq!(
            hex(&Sha256::digest(&bytes[..])),
            fixture["fragment_page_sha256"][index].as_str().unwrap()
        );
        let page = decode_page(generation(), layout, index as u64, &bytes, &control).unwrap();
        assert_eq!(page.shape().fragment_index, index as u32);
        assert_eq!(
            page.payload().len(),
            fixture["fragment_payload_bytes"][index].as_u64().unwrap() as usize
        );
        assembled.extend_from_slice(page.payload()).unwrap();
    }
    assert_eq!(&*assembled, &*encoded);
    let node = layout.decode_node(0, &assembled, &control).unwrap();
    assert_eq!(node.vector().len(), 1024);
    assert_eq!(node.vector()[1023].to_bits(), 0x8000_0000);
    assert_eq!(node.raw_norm().to_bits(), 1.0_f32.to_bits());
    assert!(node.neighbors().is_empty());
    drop((node, encoded, assembled));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn layout_arithmetic_handles_empty_large_and_boundary_generations() {
    let empty = DiskANNNodeLayout::new(3, 2, 0).unwrap();
    assert_eq!(empty.page_count(), 0);
    assert!(empty.node_address(0).is_err());
    assert!(empty.page_shape(0).is_err());
    let large = DiskANNNodeLayout::new(3, 2, u64::MAX).unwrap();
    let address = large.node_address(u64::MAX - 1).unwrap();
    let last = large.page_shape(address.first_page).unwrap();
    assert_eq!(last.first_node + u64::from(address.slot), u64::MAX - 1);
    assert!(large.node_address(u64::MAX).is_err());
    assert!(large.page_shape(large.page_count()).is_err());
    assert!(DiskANNNodeLayout::new(1024, 2, u64::MAX).is_err());
    assert!(DiskANNNodeLayout::new(0, 2, 1).is_err());
    assert!(DiskANNNodeLayout::new(3, 1, 1).is_err());
    assert!(DiskANNNodeLayout::new(3, usize::MAX, 1).is_err());
    for (database, table, index, generation) in [
        ([0; 16], 1, 1, 1),
        ([1; 16], 0, 1, 1),
        ([1; 16], 1, 0, 1),
        ([1; 16], 1, 1, 0),
    ] {
        assert!(DiskANNGeneration::new(database, table, index, generation).is_err());
    }
    assert!(DiskANNVectorVersion::new(version().writer(), 0).is_err());
}

proptest::proptest! {
    #[test]
    fn every_address_matches_its_packed_slot_or_fragment_extent(
        dimensions in 1_u32..=4096,
        degree in 2_usize..64,
        count in 1_u64..1000,
        selector in 0_u64..1000,
    ) {
        let layout = DiskANNNodeLayout::new(dimensions, degree, count).unwrap();
        let node = selector % count;
        let address = layout.node_address(node).unwrap();
        let first = layout.page_shape(address.first_page).unwrap();
        if address.fragments == 1 {
            proptest::prop_assert_eq!(first.first_node + u64::from(address.slot), node);
            proptest::prop_assert!(address.slot < first.slots);
            proptest::prop_assert_eq!(first.payload_bytes as usize, first.slots as usize * layout.slot_bytes());
        } else {
            let mut extent = 0;
            for index in 0..address.fragments {
                let shape = layout.page_shape(address.first_page + u64::from(index)).unwrap();
                proptest::prop_assert_eq!(shape.first_node, node);
                proptest::prop_assert_eq!(shape.fragment_index, index);
                extent += shape.payload_bytes as usize;
            }
            proptest::prop_assert_eq!(extent, layout.slot_bytes());
        }
    }
}
