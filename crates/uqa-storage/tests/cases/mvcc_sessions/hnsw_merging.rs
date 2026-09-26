//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent document writers share the HNSW allocator and graph without losing either input.

use super::*;
use uqa_storage::{
    key_value::KeyValueHNSWIndex, HNSWIndexParams, MemoryKeyValueStore, VectorIndex,
};

#[test]
fn independent_hnsw_writers_merge_shared_node_ids_and_preserve_serial_topology() {
    for seed in [0, 16] {
        let persistence = Persistence::new();
        let a: Arc<dyn KeyValueStore> = Arc::new(persistence.session(1 << 20));
        let b = a.open_session().unwrap();
        let reference: Arc<dyn KeyValueStore> = Arc::new(MemoryKeyValueStore::new());
        let params = HNSWIndexParams::default();
        let mut left = KeyValueHNSWIndex::create(a.clone(), "vectors", "v", 2, params).unwrap();
        let mut serial =
            KeyValueHNSWIndex::create(reference.clone(), "vectors", "v", 2, params).unwrap();
        for document in 1..=seed {
            for index in [&mut left, &mut serial] {
                index.add(document, vec![1.0, document as f32]).unwrap();
            }
        }
        left.initialize().unwrap();
        serial.initialize().unwrap();
        let mut right = KeyValueHNSWIndex::restore(b.clone(), "vectors", "v", 2, params).unwrap();
        let baseline = left.snapshot().unwrap();
        a.begin_transaction().unwrap();
        b.begin_transaction().unwrap();
        left.add_many(101, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
            .unwrap();
        right.add(102, vec![0.5, 0.5]).unwrap();
        let private = left.snapshot().unwrap();
        b.commit_transaction().unwrap();
        assert!(a.in_transaction());
        a.commit_transaction().unwrap();
        serial.add(102, vec![0.5, 0.5]).unwrap();
        serial
            .add_many(101, vec![vec![1.0, 0.0], vec![0.0, 1.0]])
            .unwrap();
        let mut expected = reference.scan_prefix(b"").unwrap();
        // The versioned owner also persists this immutable field revision. Keep the complete graph and canonical-byte comparison against the serial algorithm.
        expected.push((
            b"\0uqa-vector-field-guards-v1\0\x01v\0\0\0\x07vectors\0\0\0\x01v".to_vec(),
            vec![1],
        ));
        expected.sort();
        assert_eq!(a.scan_prefix(b"").unwrap(), expected);
        assert_eq!(left.count().unwrap(), seed as usize + 3);
        assert_eq!(right.count().unwrap(), seed as usize + 3);
        assert_eq!(baseline.count().unwrap(), seed as usize);
        assert_eq!(private.count().unwrap(), seed as usize + 2);
        assert_eq!(
            left.search_knn(&[1.0, 0.0], 100).unwrap().len(),
            seed as usize + 2
        );
    }
}
