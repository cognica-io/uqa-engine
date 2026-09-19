//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HNSW codecs retain bounded buffers and share canonical tensor fences with IVF.

use crate::{
    hnsw_index::HNSWIndex,
    key_value::{
        index_keys::{hnsw_metadata_key, hnsw_node_key, ivf_metadata_key},
        KeyValueHNSWRecords, KeyValueIVFRecords,
    },
    mvcc::{
        HNSWRecordKey, HNSWRecordLayout, HNSWRecordValue, IVFRecordKey, IVFRecordLayout,
        VersionError,
    },
    read_control::StorageReadControl,
    HNSWIndexParams, VectorIndex,
};

#[test]
fn hnsw_codecs_round_trip_exact_graphs_and_retain_their_allowance() {
    let mut index = HNSWIndex::new(4);
    for doc in 1..=8 {
        index.add(doc, vec![doc as f32, -0.0, 0.5, -1.0]).unwrap();
    }
    let snapshot = index.persistence_snapshot();
    let records = KeyValueHNSWRecords;
    let control = StorageReadControl::with_limit(1 << 20);
    for node in &snapshot.nodes {
        let key = hnsw_node_key("table\0日", "field\0本", node.node_id).unwrap();
        let encoded = records
            .encode(&key, b"", HNSWRecordValue::Node(node), &control)
            .unwrap();
        let decoded = records.node(&key, &encoded, &control).unwrap();
        assert_eq!(*decoded, *node);
        assert_eq!(decoded.raw_vector[1].to_bits(), (-0.0_f32).to_bits());
        drop((decoded, encoded));
        assert_eq!(control.memory().used(), 0);
    }
    let hnsw = hnsw_metadata_key("table\0日", "field\0本").unwrap();
    let ivf = ivf_metadata_key("table\0日", "field\0本").unwrap();
    for document in [0, 9, u64::MAX] {
        assert_eq!(
            &*records
                .key(&hnsw, HNSWRecordKey::Document(document), &control)
                .unwrap(),
            &*KeyValueIVFRecords
                .key(&ivf, IVFRecordKey::Document(document), &control)
                .unwrap()
        );
    }
    assert_eq!(
        &*records
            .key(&hnsw, HNSWRecordKey::Structure, &control)
            .unwrap(),
        &*KeyValueIVFRecords
            .key(&ivf, IVFRecordKey::Structure, &control)
            .unwrap()
    );
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn decoding_rejects_allocation_cancellation_and_invalid_graph_shapes_without_leaks() {
    let mut index = HNSWIndex::with_params(1024, HNSWIndexParams::default()).unwrap();
    index.add(1, vec![0.5; 1024]).unwrap();
    let snapshot = index.persistence_snapshot();
    let records = KeyValueHNSWRecords;
    let key = hnsw_node_key("t", "v", snapshot.nodes[0].node_id).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let encoded = records
        .encode(
            &key,
            b"",
            HNSWRecordValue::Node(&snapshot.nodes[0]),
            &control,
        )
        .unwrap();
    let empty = StorageReadControl::with_limit(0);
    let allocations = allocation_counter::measure(|| {
        assert!(matches!(
            records.node(&key, &encoded, &empty),
            Err(VersionError::Memory(_))
        ));
    });
    assert_eq!(allocations.count_total, 0);
    for limit in [128, 256, 1024] {
        let small = StorageReadControl::with_limit(limit);
        assert!(matches!(
            records.node(&key, &encoded, &small),
            Err(VersionError::Memory(_))
        ));
        assert_eq!(small.memory().used(), 0);
    }
    let cancelled = StorageReadControl::with_limit(1 << 20);
    cancelled.cancellation().cancel();
    assert!(matches!(
        records.node(&key, &encoded, &cancelled),
        Err(VersionError::Cancelled(_))
    ));
    assert_eq!(cancelled.memory().used(), 0);
    for (field, bad) in [
        ("node_id", serde_json::json!(999)),
        ("level", serde_json::json!(999)),
        ("neighbors", serde_json::json!([])),
        ("raw_vector", serde_json::json!(["invalid"])),
    ] {
        let mut data: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        data[field] = bad;
        let bytes = serde_json::to_vec(&data).unwrap();
        let limit = StorageReadControl::with_limit(1 << 20);
        assert!(records.node(&key, &bytes, &limit).is_err());
        assert_eq!(limit.memory().used(), 0);
    }
    drop(encoded);
    assert_eq!(control.memory().used(), 0);
}
