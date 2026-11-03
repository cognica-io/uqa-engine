//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph images preserve evaluated topology, pending deltas and spillable owner boundaries.

use super::*;
use crate::{
    hnsw_index::HNSWCanonicalBuilder,
    vector_index::image::{read_vector_image, tests::encode},
    HNSWIndexParams, VectorIndex,
};

fn fixture() -> Vec<u8> {
    let mut bytes = b"UQAVEC01\x02\x02\x00\x00\x00".to_vec();
    for value in [2_u64, 4, 3, 10, 0xf00d] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.push(1);
    bytes.extend(7_u64.to_le_bytes());
    for value in [0_u64, 8, 0] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.push(0);
    bytes.extend(1_u64.to_le_bytes());
    bytes.extend(7_u64.to_le_bytes());
    bytes.extend(42_u64.to_le_bytes());
    bytes.extend(0_u32.to_le_bytes());
    bytes.extend(0_u64.to_le_bytes());
    bytes.push(0);
    for (norm, values) in [(5.0_f32, [3.0_f32, 4.0]), (1.0, [0.6, 0.8])] {
        bytes.extend(norm.to_bits().to_le_bytes());
        bytes.extend(2_u64.to_le_bytes());
        for value in values {
            bytes.extend(value.to_bits().to_le_bytes());
        }
    }
    bytes.extend(0_u64.to_le_bytes()); // Empty layer zero.
    for value in [1_u64, 7, 0] {
        bytes.extend(value.to_le_bytes());
    }
    assert_eq!(bytes.len(), 196);
    bytes
}

fn restore(bytes: &[u8], dimensions: u32, control: &StorageReadControl) -> Budgeted<HNSWIndex> {
    let mut bytes = &bytes[13..];
    let mut input = ImageInput {
        reader: &mut bytes,
        control,
    };
    let result = read(&mut input, dimensions).unwrap();
    input.end().unwrap();
    result
}

#[test]
fn independent_graph_fixture_preserves_identity_norms_topology_and_pending_delta() {
    let bytes = fixture();
    let control = StorageReadControl::with_limit(1 << 20);
    let restored = restore(&bytes, 2, &control);
    assert_eq!(encode(&*restored), bytes);
    assert_eq!(restored.params().seed, 0xf00d);
    assert_eq!(
        restored.raw_vectors.get(7).unwrap().unwrap().norm.to_bits(),
        5.0_f32.to_bits()
    );
    assert_eq!(
        restored.normalized_vectors.get(7).unwrap().unwrap().values[0].to_bits(),
        0.6_f32.to_bits()
    );
    assert_eq!(
        restored
            .search_knn(&[3.0, 4.0], 1)
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .doc_id,
        42
    );
    let (mut restored, lease) = restored.into_parts();
    let delta = restored.take_persistence_delta();
    assert!(!delta.full_rewrite);
    assert_eq!(delta.meta.entry_point, Some(7));
    assert_eq!(delta.meta.next_node_id, 8);
    assert_eq!(delta.nodes().next().unwrap().unwrap().node_id, 7);
    assert_eq!(delta.nodes().count(), 1);
    assert!(restored.take_persistence_delta().nodes().next().is_none());
    restored.add(43, vec![-3.0, -4.0]).unwrap();
    assert_eq!(restored.next_node_id, 9);
    assert_eq!(
        restored.active.get(active_key(43, 0)).unwrap().as_deref(),
        Some(&8)
    );
    drop((delta, restored, lease));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn tombstones_neighbor_order_and_future_graph_deltas_survive_images() {
    let mut source = HNSWIndex::new(2);
    for id in 1..=24 {
        source
            .add_many(id, vec![vec![id as f32, 1.0], vec![1.0, id as f32]])
            .unwrap();
    }
    drop(source.take_persistence_delta());
    source.delete(7).unwrap();
    source.add(8, vec![-1.0, -0.0]).unwrap();
    assert!(source.deleted_count > 0);
    let bytes = encode(&source);
    let control = StorageReadControl::with_limit(1 << 24);
    let (mut restored, lease) = restore(&bytes, 2, &control).into_parts();
    assert_eq!(encode(&restored), bytes);
    assert_eq!(
        *source.take_persistence_delta().collect().unwrap(),
        *restored.take_persistence_delta().collect().unwrap()
    );
    for index in [&mut source, &mut restored] {
        index
            .add_many(3, vec![vec![0.0, 1.0], vec![-1.0, 0.0]])
            .unwrap();
        index.delete(14).unwrap();
    }
    assert_eq!(
        *source.take_persistence_delta().collect().unwrap(),
        *restored.take_persistence_delta().collect().unwrap()
    );
    assert_eq!(encode(&restored), encode(&source));
    drop((restored, lease));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn invalid_graph_metadata_and_edges_reject_before_candidate_publication() {
    let control = StorageReadControl::with_limit(1 << 20);
    // Invalid flags, missing entry, level mismatch, exhausted identity, wrong deletion count,
    // illegal ordinal, nonfinite raw data, normalized norm, and absent dirty-node references.
    for (offset, replacement) in [
        (53, vec![2]),
        (54, 99_u64.to_le_bytes().to_vec()),
        (62, 1_u64.to_le_bytes().to_vec()),
        (70, 7_u64.to_le_bytes().to_vec()),
        (78, 1_u64.to_le_bytes().to_vec()),
        (86, vec![2]),
        (111, 1_u32.to_le_bytes().to_vec()),
        (115, 33_u64.to_le_bytes().to_vec()),
        (123, vec![2]),
        (136, f32::NAN.to_bits().to_le_bytes().to_vec()),
        (144, 0_u32.to_le_bytes().to_vec()),
        (180, 99_u64.to_le_bytes().to_vec()),
    ] {
        let mut bytes = fixture();
        bytes[offset..offset + replacement.len()].copy_from_slice(&replacement);
        assert!(
            read_vector_image(&mut bytes.as_slice(), &control).is_err(),
            "offset {offset}"
        );
        assert_eq!(control.memory().used(), 0);
    }
    for neighbor in [7_u64, 99] {
        let mut bytes = fixture();
        bytes[164..172].copy_from_slice(&1_u64.to_le_bytes());
        bytes.splice(172..172, neighbor.to_le_bytes());
        assert!(read_vector_image(&mut bytes.as_slice(), &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let (mut source, lease) = restore(&fixture(), 2, &control).into_parts();
    source.deleted_count = 1;
    let mut output = Vec::new();
    assert!(source.write_image(&mut output, &control).is_err());
    assert!(output.is_empty());
    drop((source, lease));
    assert_eq!(control.memory().used(), 0);
}

fn vector(id: u64) -> Vec<f32> {
    (0..1024)
        .map(|coordinate| ((coordinate as u64 * 17 + id * 31) % 97) as f32 - 48.0)
        .collect()
}

#[test]
fn spilled_graph_images_use_the_invoking_workspace_and_restore_below_corpus_size() {
    let source_control = StorageReadControl::with_limit(64 << 10);
    let params = HNSWIndexParams {
        m: 2,
        ef_construction: 4,
        ef_search: 12,
        ..HNSWIndexParams::default()
    };
    let mut builder = HNSWCanonicalBuilder::new(1024, params, &source_control).unwrap();
    for id in 1..=20 {
        builder.push(id, 0, &vector(id)).unwrap();
    }
    let source = builder.finish().unwrap();
    assert!(source.raw_vectors.is_spilled());
    let expected = source.search_knn(&vector(3), 7).unwrap();
    let held = source_control
        .memory()
        .reserve(source_control.memory().limit() - source_control.memory().used())
        .unwrap();
    let bytes = encode(&*source);
    assert!(bytes.len() > 64 << 10);
    drop(held);
    let target_control = StorageReadControl::with_limit(64 << 10);
    let restored = restore(&bytes, 1024, &target_control);
    assert!(restored.raw_vectors.is_spilled());
    assert_eq!(restored.search_knn(&vector(3), 7).unwrap(), expected);
    assert_eq!(encode(&*restored), bytes);
    assert!(target_control.memory().peak() <= target_control.memory().limit());
    drop((restored, source));
    assert_eq!(source_control.memory().used(), 0);
    assert_eq!(target_control.memory().used(), 0);
}
