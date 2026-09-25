//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::native::{NativeRecord, NativeRecordIdentity};

#[test]
fn native_diskann_canonical_reads_reject_malformed_origins_and_incomplete_ordinal_sets() {
    for fault in 0..6 {
        let connection = memory();
        let control = StorageReadControl::with_limit(1 << 22);
        let source = canonical(&connection, "docs", "embedding", 2);
        let version = source
            .replace(1, &[vec![1.0, 0.0], vec![0.0, 1.0]], &control)
            .unwrap();
        let retained = source.retain(&control).unwrap();
        connection.begin_transaction().unwrap();
        connection
            .with_native_write(|snapshot, batch| {
                let owner = snapshot.table_owner("docs")?.unwrap();
                let field = ValueRef::Text(b"embedding");
                if fault == 0 {
                    snapshot.delete_prefix(
                        batch,
                        Family::VectorOrigins,
                        owner,
                        &[field, ValueRef::Integer(1)],
                    )?;
                } else if fault == 1 {
                    snapshot.delete_prefix(
                        batch,
                        Family::Vectors,
                        owner,
                        &[field, ValueRef::Integer(1), ValueRef::Integer(0)],
                    )?;
                } else {
                    let mut bytes = DiskANNCanonicalOrigin::new(version, 2, 2)?.encode();
                    match fault {
                        2 => bytes[0] = 0,
                        3 => bytes[40] = 3,
                        4 => bytes[48] = 1,
                        _ => {}
                    }
                    snapshot.put_row(
                        batch,
                        Family::VectorOrigins,
                        owner,
                        &[
                            ValueRef::Text(if fault == 5 { b"elsewhere" } else { b"docs" }),
                            field,
                            ValueRef::Integer(1),
                            ValueRef::Blob(&bytes),
                        ],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let read = source.retain(&control).unwrap();
        assert!(read.origin(1, &control).is_err(), "fault {fault}");
        let mut visited = false;
        assert!(read
            .visit_document(1, &control, &mut |_, _, _| {
                visited = true;
                Ok(())
            })
            .is_err());
        assert!(!visited);
        connection.rollback_transaction().unwrap();
        assert!(read.origin(1, &control).is_err());
        assert_eq!(retained.origin(1, &control).unwrap(), Some(version));
    }
}

#[test]
fn native_diskann_canonical_reads_validate_payload_width_finiteness_scope_and_key_identity() {
    for fault in 0..5 {
        let connection = memory();
        let control = StorageReadControl::with_limit(1 << 22);
        let source = canonical(&connection, "docs", "embedding", 2);
        source.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
        connection.begin_transaction().unwrap();
        connection
            .with_native_write(|snapshot, batch| {
                let owner = snapshot.table_owner("docs")?.unwrap();
                let key = NativeRecordIdentity::new(Family::Vectors, owner)?.encode_key(
                    &[
                        ValueRef::Text(b"embedding"),
                        ValueRef::Integer(1),
                        ValueRef::Integer(0),
                    ],
                    &snapshot.control,
                )?;
                let bytes = match fault {
                    0 => vec![0; 4],
                    1 => [f32::NAN.to_le_bytes(), 0.0_f32.to_le_bytes()].concat(),
                    2 => vec![0; 1 << 16],
                    _ => vec![0; 8],
                };
                let record = NativeRecord::encode(
                    Family::Vectors,
                    owner,
                    &[
                        ValueRef::Text(if fault == 3 { b"elsewhere" } else { b"docs" }),
                        ValueRef::Text(b"embedding"),
                        ValueRef::Integer(if fault == 4 { 2 } else { 1 }),
                        ValueRef::Integer(0),
                        ValueRef::Blob(&bytes),
                    ],
                    &snapshot.control,
                )?;
                batch.put(&key, record.row())?;
                Ok(())
            })
            .unwrap();
        let source = source.retain(&control).unwrap();
        let small = StorageReadControl::with_limit(8192);
        let mut calls = 0;
        let error = source
            .visit_document(1, &small, &mut |_, _, _| {
                calls += 1;
                Ok(())
            })
            .unwrap_err();
        assert_eq!(calls, 0, "fault {fault}: {error}");
        if fault == 2 {
            assert!(
                matches!(error, uqa_storage::StorageBackendError::Memory(_)),
                "{error}"
            );
        }
        assert_eq!(small.memory().used(), 0);
        connection.rollback_transaction().unwrap();
    }
}

#[test]
fn native_diskann_canonical_mutations_preserve_values_after_validation_and_budget_errors() {
    let unbound = ManagedConnection::open_in_memory().unwrap();
    assert!(SQLiteDiskANNCanonical::new(unbound, "docs", "embedding", 2).is_err());
    let connection = memory();
    assert!(SQLiteDiskANNCanonical::new(connection.clone(), "docs", "embedding", 0).is_err());
    let control = StorageReadControl::with_limit(1 << 22);
    let source = canonical(&connection, "docs", "embedding", 2);
    let version = source.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
    for tensor in [vec![vec![1.0]], vec![vec![f32::INFINITY, 0.0]]] {
        assert!(source.replace(1, &tensor, &control).is_err());
        assert!(!connection.in_transaction());
    }
    let tiny = StorageReadControl::with_limit(1);
    assert!(source.replace(1, &[vec![2.0, 3.0]], &tiny).is_err());
    assert_eq!(tiny.memory().used(), 0);
    assert!(!connection.in_transaction());
    assert!(source.replace(u64::MAX, &[], &control).is_err());
    assert_eq!(
        source
            .retain(&control)
            .unwrap()
            .origin(1, &control)
            .unwrap(),
        Some(version)
    );
    assert!(source.retain(&tiny).is_err());
    assert_eq!(tiny.memory().used(), 0);
    let absent = canonical(&connection, "missing", "embedding", 2)
        .retain(&control)
        .unwrap();
    canonical(&connection, "missing", "embedding", 2)
        .replace(1, &[vec![4.0, 5.0]], &control)
        .unwrap();
    assert!(absent.origin(1, &control).unwrap().is_none());
    assert_eq!(
        source
            .retain(&control)
            .unwrap()
            .origin(1, &control)
            .unwrap(),
        Some(version)
    );
}

#[test]
fn native_canonical_row_bounds_match_independent_encoded_envelope_widths() {
    let control = StorageReadControl::with_limit(8192);
    for ordinal in [false, true] {
        let mut values = vec![
            ValueRef::Text(b"table"),
            ValueRef::Text(b"field"),
            ValueRef::Integer(1),
        ];
        if ordinal {
            values.push(ValueRef::Integer(0));
        }
        values.push(ValueRef::Blob(&[0; 56]));
        let encoded = crate::mvcc::native::encode_row(&values, &control).unwrap();
        let expected = if ordinal { 105 } else { 96 };
        assert_eq!(encoded.len(), expected);
        assert_eq!(
            crate::mvcc::native::vector_row_limit(5, 5, 56, ordinal).unwrap(),
            expected
        );
    }
}
