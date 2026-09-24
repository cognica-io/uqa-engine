//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::native::NativeRecord;
use crate::vector_index::NativeIVFRecords;
use uqa_storage::mvcc::{IVFRecordKey, IVFRecordLayout};

fn owner() -> NativeRecordOwner {
    NativeRecordOwner::Object {
        identity: [3; 16],
        generation: [7; 16],
    }
}

fn header(control: &StorageReadControl) -> NativeRecord {
    let int = ValueRef::Integer;
    NativeRecord::encode(
        Family::HNSWIndexes,
        owner(),
        &[
            ValueRef::Text(b"docs\0name"),
            ValueRef::Text("埋め込み\0".as_bytes()),
            int(3),
            int(4),
            int(8),
            int(8),
            int(2),
            ValueRef::Text(b"18446744073709551615"),
            ValueRef::Null,
            int(0),
            int(0),
            int(0),
            int(0),
            int(1),
            int(1),
        ],
        control,
    )
    .unwrap()
}

#[test]
fn native_hnsw_codec_preserves_rows_and_shares_canonical_vector_guards() {
    let control = StorageReadControl::with_limit(1 << 20);
    let codec = NativeHNSWRecords;
    let header = header(&control);
    let decoded = codec.header(header.key(), header.row(), &control).unwrap();
    assert_eq!(decoded.params.seed, u64::MAX);
    assert_eq!(decoded.revision, Some(1));
    let updated = codec
        .encode(
            header.key(),
            header.row(),
            Value::Header {
                meta: decoded.meta,
                revision: Some(9),
            },
            &control,
        )
        .unwrap();
    assert_eq!(
        codec
            .header(header.key(), &updated, &control)
            .unwrap()
            .revision,
        Some(9)
    );
    let ivf = Identity::new(Family::IVFIndexes, owner())
        .unwrap()
        .encode_key(&[ValueRef::Text("埋め込み\0".as_bytes())], &control)
        .unwrap();
    for (hnsw, ivf_address) in [
        (Key::Structure, IVFRecordKey::Structure),
        (Key::Document(0), IVFRecordKey::Document(0)),
        (
            Key::Document(i64::MAX as u64),
            IVFRecordKey::Document(i64::MAX as u64),
        ),
        (Key::Vectors, IVFRecordKey::Vectors),
    ] {
        assert_eq!(
            &*codec.key(header.key(), hnsw, &control).unwrap(),
            &*NativeIVFRecords.key(&ivf, ivf_address, &control).unwrap()
        );
    }
    check_nodes_and_edges(&header, &control);
    for invalid in [
        Key::Document(u64::MAX),
        Key::Node(u64::MAX),
        Key::Edge {
            source: 1,
            layer: usize::MAX,
            target: 2,
        },
        Key::Edge {
            source: 1,
            layer: 0,
            target: u64::MAX,
        },
    ] {
        assert!(codec.key(header.key(), invalid, &control).is_err());
    }
    for revision in [None, Some(u64::MAX)] {
        assert!(codec
            .encode(
                header.key(),
                header.row(),
                Value::Header {
                    meta: decoded.meta,
                    revision
                },
                &control
            )
            .is_err());
    }
}

fn check_nodes_and_edges(header: &NativeRecord, control: &StorageReadControl) {
    let codec = NativeHNSWRecords;
    let mut node = HNSWNodeSnapshot {
        node_id: 17,
        doc_id: 101,
        vector_ordinal: 2,
        level: 2,
        deleted: true,
        raw_vector: vec![1.0, -0.0, 2.0],
        neighbors: vec![vec![18], vec![], vec![]],
    };
    let key = codec.key(header.key(), Key::Node(17), control).unwrap();
    let value = codec
        .encode(&key, header.row(), Value::Node(&node), control)
        .unwrap();
    let read = codec.node(&key, &value, control).unwrap();
    assert_eq!(
        (
            read.node_id,
            read.doc_id,
            read.vector_ordinal,
            read.level,
            read.deleted
        ),
        (17, 101, 2, 2, true)
    );
    assert_eq!(read.raw_vector[1].to_bits(), (-0.0_f32).to_bits());
    assert_eq!(read.neighbors, vec![Vec::<u64>::new(); 3]);
    let edge = codec
        .key(
            header.key(),
            Key::Edge {
                source: 17,
                layer: 0,
                target: 18,
            },
            control,
        )
        .unwrap();
    let value = codec
        .encode(&edge, header.row(), Value::Edge, control)
        .unwrap();
    assert_eq!(codec.edge(&edge, &value, control).unwrap(), (17, 0, 18));
    for source in [None, Some(17)] {
        assert!(edge.starts_with(
            &codec
                .edges_prefix(header.key(), source, control)
                .unwrap()
                .unwrap()
        ));
    }
    assert_eq!(
        &*codec.metadata_key(&edge, control).unwrap().unwrap(),
        header.key()
    );
    let (_, row) = decode_record(&edge, &value, control).unwrap();
    assert_eq!(
        &row[..2],
        &[
            ValueRef::Text(b"docs\0name"),
            ValueRef::Text("埋め込み\0".as_bytes())
        ]
    );
    node.node_id = 18;
    assert!(codec
        .encode(&key, header.row(), Value::Node(&node), control)
        .is_err());
    node.node_id = 17;
    node.level = usize::MAX;
    assert!(codec
        .encode(&key, header.row(), Value::Node(&node), control)
        .is_err());
}

#[test]
fn native_hnsw_codec_rejects_malformed_nodes_and_releases_failed_decode_memory() {
    let control = StorageReadControl::with_limit(1 << 20);
    let int = ValueRef::Integer;
    let codec = NativeHNSWRecords;
    let mut values = [
        ValueRef::Text(b"docs"),
        ValueRef::Text(b"embedding"),
        int(1),
        int(7),
        int(0),
        int(0),
        int(0),
        ValueRef::Blob(&[0; 12]),
    ];
    for (column, invalid) in [
        (2, int(-1)),
        (3, int(-1)),
        (4, int(-1)),
        (4, int(i64::MAX)),
        (5, int(i64::MAX)),
        (6, int(2)),
        (7, ValueRef::Blob(&[0; 11])),
    ] {
        let previous = values[column];
        values[column] = invalid;
        let record = NativeRecord::encode(Family::HNSWNodes, owner(), &values, &control).unwrap();
        let before = control.memory().used();
        assert!(codec.node(record.key(), record.row(), &control).is_err());
        assert_eq!(control.memory().used(), before);
        values[column] = previous;
    }
    let record = NativeRecord::encode(Family::HNSWNodes, owner(), &values, &control).unwrap();
    for limit in [0, 1] {
        let limited = StorageReadControl::with_limit(limit);
        let allocation = allocation_counter::measure(|| {
            assert!(matches!(
                codec.node(record.key(), record.row(), &limited),
                Err(VersionError::Memory(_))
            ));
        });
        assert_eq!(allocation.bytes_total, 0);
        assert_eq!(limited.memory().used(), 0);
    }
    let cancelled = StorageReadControl::with_limit(0);
    cancelled.cancellation().cancel();
    assert!(matches!(
        codec.node(record.key(), record.row(), &cancelled),
        Err(VersionError::Cancelled(_))
    ));
    for length in 0..record.key().len() {
        assert!(codec
            .node(&record.key()[..length], record.row(), &control)
            .is_err());
    }
}
