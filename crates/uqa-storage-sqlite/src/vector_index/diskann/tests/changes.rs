//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::native::{NativeRecord, NativeRecordIdentity};
use uqa_storage::VectorIndex;

#[test]
fn native_diskann_changes_stream_current_origins_across_large_history_and_private_undo() {
    let connection = memory();
    let control = StorageReadControl::with_limit(1 << 22);
    let source = canonical(&connection, "docs", "embedding", 2);
    let first = source.replace(0, &[vec![1.0, 0.0]], &control).unwrap();
    let original = source.replace(5, &[], &control).unwrap();
    let terminal = source
        .replace(i64::MAX as DocId, &[vec![0.0, 0.0]], &control)
        .unwrap();
    let retained = source.retain(&control).unwrap();
    connection.begin_transaction().unwrap();
    connection.savepoint("before").unwrap();
    let undone = source.replace(5, &[vec![0.0, 1.0]], &control).unwrap();
    let private = source.retain(&control).unwrap();
    connection.rollback_to_savepoint("before").unwrap();
    let mut current = original;
    for _ in 0..256 {
        current = source.replace(5, &[], &control).unwrap();
    }
    assert_ne!(current, undone);
    connection.commit_transaction().unwrap();
    assert_eq!(change_count(&connection), 259);
    let small = StorageReadControl::with_limit(8192);
    for (view, middle) in [
        (&retained, original),
        (&private, undone),
        (&source.retain(&control).unwrap(), current),
    ] {
        assert_change(view, 0, first, &small);
        assert_change(view, 5, middle, &small);
        assert_change(view, i64::MAX as DocId, terminal, &small);
        assert!(view
            .next_change_after(Some(i64::MAX as DocId), &small)
            .unwrap()
            .is_none());
        assert_eq!(small.memory().used(), 0);
    }
    let tiny = StorageReadControl::with_limit(1);
    assert!(retained.next_change_after(None, &tiny).is_err());
    assert_eq!(tiny.memory().used(), 0);
    let cancelled = StorageReadControl::with_limit(8192);
    cancelled.cancellation().cancel();
    for after in [None, Some(i64::MAX as DocId)] {
        assert!(retained.next_change_after(after, &cancelled).is_err());
    }
    let original_control = StorageReadControl::with_limit(8192);
    let cancelled_source = source.retain(&original_control).unwrap();
    original_control.cancellation().cancel();
    assert!(cancelled_source.next_change_after(None, &small).is_err());
    let absent = canonical(&connection, "absent", "embedding", 2)
        .retain(&control)
        .unwrap();
    assert!(absent.next_change_after(None, &cancelled).is_err());
}

#[test]
fn native_diskann_changes_preserve_late_commits_without_writer_order_coverage() {
    let connection = memory();
    let control = StorageReadControl::with_limit(1 << 22);
    let source = canonical(&connection, "docs", "embedding", 2);
    source.replace(0, &[], &control).unwrap();
    let peer = connection.new_session();
    connection.begin_transaction().unwrap();
    let late = source.replace(7, &[vec![1.0, 0.0]], &control).unwrap();
    let early = canonical(&peer, "docs", "embedding", 2)
        .replace(9, &[], &control)
        .unwrap();
    assert!(late.writer().allocation() < early.writer().allocation());
    let before = canonical(&peer, "docs", "embedding", 2)
        .retain(&control)
        .unwrap();
    connection.commit_transaction().unwrap();
    assert_eq!(
        before.next_change_after(Some(0), &control).unwrap(),
        Some(DiskANNChangeIdentity::new(9, early))
    );
    let after = source.retain(&control).unwrap();
    assert_change(&after, 7, late, &control);
    assert_change(&after, 9, early, &control);
}

#[test]
fn native_diskann_changes_skip_deleted_documents_and_clear_their_field_namespace() {
    let connection = memory();
    let control = StorageReadControl::with_limit(1 << 22);
    let source = canonical(&connection, "docs", "embedding", 2);
    let old = source.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
    let empty = source.replace(2, &[], &control).unwrap();
    let retained = source.retain(&control).unwrap();
    let mut legacy = SQLiteVectorIndex::new(connection.clone(), "docs", "embedding", 2);
    connection.begin_transaction().unwrap();
    legacy.add(1, vec![0.0, 1.0]).unwrap();
    assert!(source
        .retain(&control)
        .unwrap()
        .next_change_after(None, &control)
        .is_err());
    legacy.delete(1).unwrap();
    assert_eq!(
        source
            .retain(&control)
            .unwrap()
            .next_change_after(None, &control)
            .unwrap(),
        Some(DiskANNChangeIdentity::new(2, empty))
    );
    legacy.clear().unwrap();
    assert!(source
        .retain(&control)
        .unwrap()
        .next_change_after(None, &control)
        .unwrap()
        .is_none());
    connection.commit_transaction().unwrap();
    assert_eq!(change_count(&connection), 0);
    assert_change(&retained, 1, old, &control);
}

#[test]
fn native_diskann_changes_reject_malformed_keys_and_bounded_current_rows() {
    for fault in 0..6 {
        let connection = memory();
        let control = StorageReadControl::with_limit(1 << 22);
        let source = canonical(&connection, "docs", "embedding", 2);
        let version = source.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
        let identity = DiskANNChangeIdentity::new(1, version).encode();
        connection.begin_transaction().unwrap();
        connection
            .with_native_write(|snapshot, batch| {
                let owner = snapshot.table_owner("docs")?.unwrap();
                let family = NativeRecordIdentity::new(Family::VectorChanges, owner)?;
                let field = ValueRef::Text(b"embedding");
                let key =
                    family.encode_key(&[field, ValueRef::Blob(&identity)], &snapshot.control)?;
                let bad_identity = DiskANNChangeIdentity::new(0, version).encode();
                let payload = if fault == 1 {
                    vec![0; 1 << 16]
                } else {
                    DiskANNCanonicalOrigin::new(version, 2, u64::from(fault != 0))?
                        .encode()
                        .to_vec()
                };
                let record = NativeRecord::encode(
                    Family::VectorChanges,
                    owner,
                    &[
                        ValueRef::Text(if fault == 2 { b"elsewhere" } else { b"docs" }),
                        field,
                        ValueRef::Blob(if fault == 3 { &bad_identity } else { &identity }),
                        ValueRef::Blob(&payload),
                    ],
                    &snapshot.control,
                )?;
                if fault >= 4 {
                    let invalid_identity = if fault == 4 {
                        vec![0]
                    } else {
                        DiskANNChangeIdentity::new(u64::MAX, version)
                            .encode()
                            .to_vec()
                    };
                    let key = family.encode_key(
                        &[field, ValueRef::Blob(&invalid_identity)],
                        &snapshot.control,
                    )?;
                    batch.put(&key, record.row())?;
                } else {
                    batch.put(&key, record.row())?;
                }
                Ok(())
            })
            .unwrap();
        let retained = source.retain(&control).unwrap();
        let small = StorageReadControl::with_limit(8192);
        let error = retained
            .next_change_after(if fault == 5 { Some(1) } else { None }, &small)
            .unwrap_err();
        if fault == 1 {
            assert!(
                matches!(error, uqa_storage::StorageBackendError::Memory(_)),
                "{error}"
            );
        }
        assert_eq!(small.memory().used(), 0);
        connection.rollback_transaction().unwrap();
        assert_change(&source.retain(&control).unwrap(), 1, version, &control);
    }
}

#[test]
fn native_diskann_change_row_bound_matches_independent_envelope_bytes() {
    let control = StorageReadControl::with_limit(8192);
    let row = [
        ValueRef::Text(b"table"),
        ValueRef::Text(b"field"),
        ValueRef::Blob(&[0; 40]),
        ValueRef::Blob(&[0; 56]),
    ];
    assert_eq!(
        crate::mvcc::native::encode_row(&row, &control)
            .unwrap()
            .len(),
        132
    );
    assert_eq!(
        crate::mvcc::native::variable_fields_limit(&[5, 5, 40, 56]).unwrap(),
        132
    );
}

#[test]
fn native_diskann_change_completion_retry_keeps_one_durable_mutation() {
    let connection = memory();
    let control = StorageReadControl::with_limit(1 << 22);
    connection.with_physical(|sqlite| {
        sqlite.execute_batch("CREATE TRIGGER reject_change_ack BEFORE UPDATE ON _uqa_mvcc_transactions WHEN NEW.status=4 BEGIN SELECT RAISE(ABORT, 'injected change acknowledgement failure'); END;")?;
        Ok(())
    }).unwrap();
    let error = canonical(&connection, "docs", "embedding", 2)
        .replace(1, &[vec![3.0, 4.0]], &control)
        .unwrap_err();
    let Some(uqa_storage::mvcc::CommitErrorOutcome::Committed(receipt)) = error.commit_outcome()
    else {
        panic!("durable publication outcome was lost: {error}");
    };
    assert!(connection.in_transaction());
    let peer = connection.new_session();
    let source = canonical(&peer, "docs", "embedding", 2)
        .retain(&control)
        .unwrap();
    let version = source.origin(1, &control).unwrap().unwrap();
    assert_eq!(version.writer(), receipt.transaction);
    assert_eq!(version.revision(), 1);
    assert_change(&source, 1, version, &control);
    assert_eq!(change_count(&peer), 1);
    peer.with_physical(|sqlite| {
        sqlite.execute_batch("DROP TRIGGER reject_change_ack")?;
        Ok(())
    })
    .unwrap();
    connection.commit_transaction().unwrap();
    assert!(!connection.in_transaction());
    assert_eq!(change_count(&connection), 1);
    let current = canonical(&connection, "docs", "embedding", 2)
        .retain(&control)
        .unwrap();
    assert_change(&current, 1, version, &control);
    assert_tensor(&current, 1, &[vec![3.0, 4.0]], &control);
}
