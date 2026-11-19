//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::spill_map::Record;

#[test]
fn topology_record_windows_preserve_layers_and_retained_identity_without_decoding_nodes() {
    for resident in [0, 1 << 20] {
        let source = StorageReadControl::with_limit(4 << 20);
        let mut map = Map::new(source.memory(), resident);
        let node = HNSWNode {
            id: u64::MAX - 1,
            doc_id: u64::MAX,
            vector_ordinal: u32::MAX,
            level: 2,
            deleted: true,
            neighbors: vec![(0..1500).collect(), vec![], (4000..5100).collect()],
        };
        map.insert(7, node.clone(), Some(&source)).unwrap();
        let retained = map.clone();
        map.insert(
            7,
            HNSWNode {
                id: 99,
                neighbors: vec![vec![], vec![], vec![]],
                ..node.clone()
            },
            Some(&source),
        )
        .unwrap();
        let invoking = StorageReadControl::with_limit(0);
        let held = source
            .memory()
            .reserve(source.memory().available())
            .unwrap();
        let mut output = [0_u64; 1024];
        let allocation = allocation_counter::measure(|| {
            let view = NodeRead::open(&retained, 7, &invoking).unwrap().unwrap();
            assert_eq!(view.id, node.id);
            assert_eq!(view.doc_id, node.doc_id);
            assert_eq!(view.vector_ordinal, node.vector_ordinal);
            assert_eq!(view.level, node.level);
            assert_eq!(view.deleted, node.deleted);
            assert_eq!(view.layer_count(), 3);
            for (layer, expected) in node.neighbors.iter().enumerate() {
                assert_eq!(view.neighbor_count(layer).unwrap(), expected.len());
                for width in [1, 7, 512, 1024] {
                    let mut offset = 0;
                    while offset < expected.len() {
                        let count = view
                            .read_neighbors(layer, offset, &mut output[..width], &invoking)
                            .unwrap();
                        assert_eq!(count, width.min(512).min(expected.len() - offset));
                        assert_eq!(output[..count], expected[offset..offset + count]);
                        offset += count;
                    }
                    assert_eq!(
                        view.read_neighbors(layer, offset, &mut output, &invoking)
                            .unwrap(),
                        0
                    );
                }
            }
        });
        assert_eq!(allocation.bytes_total, 0);
        let view = NodeRead::open(&retained, 7, &invoking).unwrap().unwrap();
        assert!(view.neighbor_count(3).is_err());
        assert!(view
            .read_neighbors(0, 1501, &mut output, &invoking)
            .is_err());
        assert!(NodeRead::open(&retained, 8, &invoking).unwrap().is_none());
        invoking.cancellation().cancel();
        assert!(view.read_neighbors(1, 0, &mut [], &invoking).is_err());
        assert!(NodeRead::open(&retained, 7, &invoking).is_err());
        drop((held, map, retained));
        assert_eq!(source.memory().used(), 0);
    }
}

#[derive(Clone)]
struct Encoded(Vec<u8>);

impl Record for Encoded {
    fn memory_bytes(&self) -> StorageBackendResult<usize> {
        Ok(size_of::<Self>() + self.0.capacity())
    }

    fn encoded_bytes(&self) -> StorageBackendResult<usize> {
        Ok(self.0.len())
    }

    fn encode(&self, bytes: &mut Vec<u8>) {
        bytes.extend_from_slice(&self.0);
    }

    fn decode(bytes: &[u8]) -> StorageBackendResult<Self> {
        Ok(Self(bytes.to_vec()))
    }
}

#[test]
fn topology_record_views_retain_the_existing_decoder_rejection_order() {
    let source = StorageReadControl::with_limit(4 << 20);
    let invoking = StorageReadControl::with_limit(0);
    let mut map = Map::new(source.memory(), 0);
    let node = HNSWNode {
        id: 9,
        doc_id: 12,
        vector_ordinal: 3,
        level: 1,
        deleted: false,
        neighbors: vec![vec![1, 4, 7], vec![4]],
    };
    let mut encoded = Vec::new();
    node.encode(&mut encoded);
    let mut cases: Vec<_> = (0..=encoded.len())
        .map(|end| encoded[..end].to_vec())
        .collect();
    for (offset, replacement) in [(20, u64::MAX), (29, u64::MAX), (37, u64::MAX)] {
        let mut bytes = encoded.clone();
        bytes[offset..offset + 8].copy_from_slice(&replacement.to_le_bytes());
        cases.push(bytes);
    }
    let mut flag = encoded.clone();
    flag[28] = 2;
    cases.push(flag);
    let mut trailing = encoded;
    trailing.push(1);
    cases.push(trailing);
    for (key, bytes) in cases.into_iter().enumerate() {
        let expected = HNSWNode::decode(&bytes)
            .err()
            .map(|error| error.to_string());
        map.insert(key as u128, Encoded(bytes), Some(&source))
            .unwrap();
        let RecordRead::Spilled(record) = map.read_record(key as u128, &invoking).unwrap().unwrap()
        else {
            panic!("zero resident allowance must retain the encoded record");
        };
        let actual = NodeRead::spilled(record, &invoking)
            .err()
            .map(|error| error.to_string());
        assert_eq!(actual, expected, "case {key}");
    }
}
