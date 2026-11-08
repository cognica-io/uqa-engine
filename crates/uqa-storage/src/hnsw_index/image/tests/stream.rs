//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{image_io::ImageReader, StorageBackendError};

fn prepare(reader: &mut ImageReader<'_>, maximum: usize) -> usize {
    let mut positions = 0;
    loop {
        let step = reader.prepare(maximum).unwrap();
        assert!(step.positions <= maximum);
        assert!(step.ready || step.positions != 0);
        positions += step.positions;
        if step.ready {
            break;
        }
    }
    assert_eq!(reader.prepare(1).unwrap().positions, 0);
    positions
}

fn output(reader: &mut ImageReader<'_>, width: usize) -> Vec<u8> {
    let mut result = Vec::new();
    let mut buffer = vec![0; width];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        assert!(count <= width.min(16 * 1024));
        if count == 0 {
            return result;
        }
        result.extend_from_slice(&buffer[..count]);
    }
}

#[test]
fn hnsw_image_continuations_preserve_independent_bytes_and_owned_source_retirement() {
    for expected in [fixture(), topology_fixture()] {
        verify_fixture(&expected);
    }
}

fn topology_fixture() -> Vec<u8> {
    let mut bytes = b"UQAVEC01\x02\x02\x00\x00\x00".to_vec();
    for value in [2_u64, 4, 3, 10, 0xf00d] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.push(1);
    for value in [7_u64, 1, 20, 1] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.push(0);
    bytes.extend(3_u64.to_le_bytes());
    for (id, document, level, deleted, norm, raw, normalized, neighbors) in [
        (
            7_u64,
            42_u64,
            1_u64,
            false,
            5.0_f32,
            [3.0_f32, 4.0],
            [0.6_f32, 0.8],
            vec![vec![19_u64, 11], vec![11]],
        ),
        (
            11,
            43,
            1,
            false,
            1.0,
            [-0.0, 1.0],
            [-0.0, 1.0],
            vec![vec![7, 19], vec![7]],
        ),
        (
            19,
            44,
            0,
            true,
            1.0,
            [1.0, 0.0],
            [1.0, 0.0],
            vec![vec![11, 7]],
        ),
    ] {
        bytes.extend(id.to_le_bytes());
        bytes.extend(document.to_le_bytes());
        bytes.extend(0_u32.to_le_bytes());
        bytes.extend(level.to_le_bytes());
        bytes.push(u8::from(deleted));
        for (norm, values) in [(norm, raw), (1.0, normalized)] {
            bytes.extend(norm.to_bits().to_le_bytes());
            bytes.extend(2_u64.to_le_bytes());
            for value in values {
                bytes.extend(value.to_bits().to_le_bytes());
            }
        }
        for layer in neighbors {
            bytes.extend((layer.len() as u64).to_le_bytes());
            for id in layer {
                bytes.extend(id.to_le_bytes());
            }
        }
    }
    for value in [2_u64, 7, 0, 19, 1] {
        bytes.extend(value.to_le_bytes());
    }
    bytes
}

fn verify_fixture(expected: &[u8]) {
    let source_control = StorageReadControl::with_limit(1 << 20);
    let index = restore(expected, 2, &source_control);
    let original = StorageReadControl::with_limit(1 << 20);
    let snapshot = index.capture_image_with_control(&original).unwrap();
    drop(index);
    assert_eq!(source_control.memory().used(), 0);
    let mut visited = None;
    for (width, maximum) in [(1, 1), (3, 2), (13, 7), (95, 128), (4093, 3), (32 << 10, 1)] {
        let invoking = StorageReadControl::with_limit(64 << 10).with_temporary_limit(1 << 20);
        let mut reader = snapshot.image_reader(&invoking).unwrap();
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        let positions = prepare(&mut reader, maximum);
        assert_eq!(*visited.get_or_insert(positions), positions);
        assert_eq!(output(&mut reader, width), expected);
        assert_eq!(invoking.memory().used(), 0);
    }
    let invoking = StorageReadControl::with_limit(128 << 10);
    let mut reader = snapshot.into_image_reader(&invoking).unwrap();
    let bytes = std::thread::spawn(move || {
        prepare(&mut reader, 1);
        output(&mut reader, 7)
    })
    .join()
    .unwrap();
    assert_eq!(bytes, expected);
    assert_eq!(original.memory().used(), 0);
    assert_eq!(invoking.memory().used(), 0);
}

#[test]
fn hnsw_image_continuations_stream_large_spilled_vectors_without_output_allocation() {
    let source = StorageReadControl::with_limit(32 << 20);
    let dimensions = 1 << 19;
    let mut index =
        HNSWIndex::with_memory(dimensions, HNSWIndexParams::default(), source.memory()).unwrap();
    index.add(7, vec![-0.0; dimensions as usize]).unwrap();
    assert!(index.raw_vectors.is_spilled());
    let expected = encode(&index);
    let held = source
        .memory()
        .reserve(source.memory().available())
        .unwrap();
    let invoking = StorageReadControl::with_limit(64 << 10).with_temporary_limit(1 << 20);
    let mut reader = index.image_reader(&invoking).unwrap();
    let positions = prepare(&mut reader, 1);
    assert!((1024..1100).contains(&positions));
    let workspace = invoking
        .memory()
        .reserve(invoking.memory().available() - 1024)
        .unwrap();
    let mut buffer = vec![0; 32 << 10];
    let mut emitted = 0;
    let allocation = allocation_counter::measure(|| loop {
        let count = reader.read(&mut buffer).unwrap();
        assert!(count <= 16 << 10);
        if count == 0 {
            break;
        }
        assert_eq!(&buffer[..count], &expected[emitted..emitted + count]);
        emitted += count;
    });
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(emitted, expected.len());
    assert!(emitted > 4 << 20);
    drop(reader);
    assert_eq!(invoking.memory().used(), workspace.bytes());
    drop((workspace, held, index));
    assert_eq!(invoking.memory().used(), 0);
    assert_eq!(source.memory().used(), 0);
}

#[test]
fn hnsw_image_continuations_bound_empty_vectors_and_spilled_topology() {
    let source = StorageReadControl::with_limit(128 << 10);
    let index = crate::hnsw_index::validation::tests::chain(256, &source);
    let expected = encode(&index);
    let held = source
        .memory()
        .reserve(source.memory().available())
        .unwrap();
    let invoking = StorageReadControl::with_limit(64 << 10).with_temporary_limit(4 << 20);
    let mut reader = index.image_reader(&invoking).unwrap();
    let visited = prepare(&mut reader, 1);
    assert!(visited > 256 * 10 && visited < 256 * 50);
    assert_eq!(output(&mut reader, 127), expected);
    assert_eq!(invoking.memory().used(), 0);
    assert_eq!(invoking.file_budget().unwrap().usage().0, 0);
    assert!(invoking.file_budget().unwrap().usage().1 > 0);
    drop(reader);
    drop((held, index));
    assert_eq!(source.memory().used(), 0);
}

#[test]
fn hnsw_image_continuations_fail_before_output_and_retire_each_cancellation_owner() {
    let source = StorageReadControl::with_limit(1 << 20);
    let mut index = restore(&fixture(), 2, &source);
    let original = StorageReadControl::with_limit(1 << 20);
    let snapshot = index.capture_image_with_control(&original).unwrap();
    index.clear().unwrap();
    drop(index);
    for cancel_original in [false, true] {
        for before_output in [false, true] {
            let invoking = StorageReadControl::with_limit(64 << 10);
            let mut reader = snapshot.image_reader(&invoking).unwrap();
            if before_output {
                prepare(&mut reader, 1);
            } else {
                assert!(!reader.prepare(1).unwrap().ready);
            }
            let token = if cancel_original {
                original.cancellation()
            } else {
                invoking.cancellation()
            };
            token.cancel();
            let mut bytes = [99; 16];
            assert!(matches!(
                reader.read(&mut bytes),
                Err(StorageBackendError::Cancelled(_))
            ));
            assert_eq!(bytes, [99; 16]);
            assert_eq!(invoking.memory().used(), 0);
            token.reset();
            assert!(reader.prepare(1).is_err());
            assert!(reader.read(&mut bytes).is_err());
        }
    }
    drop(snapshot);
    assert_eq!(original.memory().used(), 0);
    let mut index = HNSWIndex::new(2);
    index.add(1, vec![1.0, 0.0]).unwrap();
    let id = *index.active.get(active_key(1, 0)).unwrap().unwrap();
    let mut node = (*index.nodes.get(u128::from(id)).unwrap().unwrap()).clone();
    node.neighbors[0].push(id);
    index.nodes.insert(u128::from(id), node, None).unwrap();
    let invoking = StorageReadControl::with_limit(64 << 10);
    let mut reader = index.image_reader(&invoking).unwrap();
    let mut bytes = [99; 16];
    assert!(reader
        .read(&mut bytes)
        .unwrap_err()
        .to_string()
        .contains("duplicate or self edges"));
    assert_eq!(bytes, [99; 16]);
    assert_eq!(invoking.memory().used(), 0);
}
