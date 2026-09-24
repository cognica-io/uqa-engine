//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::native::NativeRecord;
use uqa_storage::ivf_index::{IVFMetadataSnapshot, IVFState};

fn owner() -> NativeRecordOwner {
    NativeRecordOwner::Object {
        identity: [3; 16],
        generation: [7; 16],
    }
}

#[test]
fn native_ivf_codec_roundtrips_existing_rows_and_checks_numeric_bounds() {
    let control = StorageReadControl::with_limit(1 << 20);
    let rows = NativeIVFRecords;
    let header = NativeRecord::encode(
        Family::IVFIndexes,
        owner(),
        &[
            ValueRef::Text(b"docs\0name"),
            ValueRef::Text("埋め込み\0".as_bytes()),
            ValueRef::Integer(2),
            ValueRef::Integer(2),
            ValueRef::Integer(2),
            ValueRef::Integer(3),
            ValueRef::Text(b"untrained"),
            ValueRef::Integer(0),
            ValueRef::Integer(0),
            ValueRef::Integer(0),
        ],
        &control,
    )
    .unwrap();
    let decoded = rows.header(header.key(), header.row(), &control).unwrap();
    assert_eq!(decoded.revision, None);
    let snapshot = IVFMetadataSnapshot {
        state: IVFState::Trained,
        centroids: vec![vec![1.0, 0.0]],
        assignments: vec![(3, 0, 0)],
        trained_size: 1,
        deletes_since_train: 0,
        vector_count: 1,
    };
    let encoded = rows
        .encode(
            header.key(),
            header.row(),
            Value::Header {
                snapshot: &snapshot,
                revision: None,
            },
            &control,
        )
        .unwrap();
    assert_eq!(
        rows.header(header.key(), &encoded, &control).unwrap().state,
        IVFState::Trained
    );
    for key in [
        Key::Structure,
        Key::Document(0),
        Key::Document(i64::MAX as u64),
        Key::Centroid(0),
        Key::Assignment(3, 0),
    ] {
        let encoded = rows.key(header.key(), key, &control).unwrap();
        Identity::visit_key_components(&encoded, &control, |_, _| Ok(())).unwrap();
    }
    assert!(rows
        .key(header.key(), Key::Document(u64::MAX), &control)
        .is_err());
    let key = rows.key(header.key(), Key::Centroid(0), &control).unwrap();
    let encoded = rows
        .encode(&key, header.row(), Value::Centroid(&[1.0, 0.0]), &control)
        .unwrap();
    let (id, vector) = rows.centroid(&key, &encoded, &control).unwrap();
    assert_eq!(id, 0);
    assert_eq!(&*vector, &[1.0, 0.0]);
    let key = rows
        .key(header.key(), Key::Assignment(3, 0), &control)
        .unwrap();
    let encoded = rows
        .encode(&key, header.row(), Value::Assignment(0), &control)
        .unwrap();
    assert_eq!(
        rows.assignment(&key, &encoded, &control).unwrap(),
        (3, 0, 0)
    );
    assert!(rows
        .encode(&key, header.row(), Value::Assignment(usize::MAX), &control)
        .is_err());
}

#[test]
fn native_ivf_codec_rejects_malformed_vectors_before_unbounded_allocation() {
    let control = StorageReadControl::with_limit(1 << 20);
    let rows = NativeIVFRecords;
    for (document, order, payload) in [(-1, 0, vec![0; 8]), (1, -1, vec![0; 8]), (1, 0, vec![0; 7])]
    {
        let record = NativeRecord::encode(
            Family::Vectors,
            owner(),
            &[
                ValueRef::Text(b"docs"),
                ValueRef::Text(b"embedding"),
                ValueRef::Integer(document),
                ValueRef::Integer(order),
                ValueRef::Blob(&payload),
            ],
            &control,
        )
        .unwrap();
        assert!(rows.vector(record.key(), record.row(), &control).is_err());
        let limited = StorageReadControl::with_limit(1);
        let allocation = allocation_counter::measure(|| {
            assert!(rows.vector(record.key(), record.row(), &limited).is_err());
        });
        assert_eq!(allocation.bytes_total, 0);
        assert_eq!(limited.memory().used(), 0);
        let cancelled = StorageReadControl::with_limit(1 << 20);
        cancelled.cancellation().cancel();
        assert!(rows.vector_id(record.key(), &cancelled).is_err());
        assert_eq!(cancelled.memory().used(), 0);
    }
}
