//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Catalog, ManagedConnection, SQLiteDocumentStore};
use uqa_storage::{
    mvcc::VersionedSessionOptions, read_control::StorageReadControl, DocumentStore, StoredDocument,
};

fn fixture() -> SQLiteDocumentStore {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 512 << 10,
        })
        .unwrap();
    SQLiteDocumentStore::new(connection, "docs")
}

#[test]
fn requested_projections_keep_spilled_rows_through_reentrant_writes_and_rollback() {
    let mut store = fixture();
    for id in [0, 9999] {
        store
            .put(
                id,
                Document::from([("value".into(), Value::Int(id as i64))]),
            )
            .unwrap();
    }
    store.conn.begin_transaction().unwrap();
    for id in 1..=96 {
        let mut fields = Document::from([
            ("value".into(), Value::Int(id as i64)),
            ("body".into(), Value::Str("x".repeat(4096))),
        ]);
        if id == 17 {
            fields.insert("bytes".into(), Value::Bytes(vec![17; 8192]));
        }
        store
            .put_stored(
                id,
                StoredDocument::with_metadata(fields, DocumentMetadata::with_tuple_xmin(42)),
            )
            .unwrap();
    }
    store.delete(32).unwrap();
    let captured = store.conn.native_snapshot().unwrap().unwrap();
    let control = StorageReadControl::with_limit(256 << 10);
    let read = NativeDocumentRead::with_control(&captured, "docs", &control, None).unwrap();
    let key = read.selection_key(1).unwrap().unwrap();
    {
        let mut cursor = captured.view.private_cursor(&key, None, &control).unwrap();
        let entry = cursor.next(&control).unwrap().unwrap();
        assert_eq!(entry.key(), &*key);
        assert!(
            matches!(
                entry.read(&StorageReadControl::with_limit(0)),
                Err(VersionError::Memory(_))
            ),
            "the selected document must actually be spilled"
        );
    }
    drop(key);

    let ids = [0, 1, 17, 32, 96, 97, 9999, 17, 1];
    let mut seen = Vec::new();
    read.visit_projection(
        &ids,
        &["value", "bytes", "value"],
        &mut |id, present, values| {
            assert_eq!(present, !matches!(id, 32 | 97));
            assert_eq!(
                values[0],
                &if present {
                    Value::Int(id as i64)
                } else {
                    Value::Null
                }
            );
            assert!(std::ptr::eq(values[0], values[2]));
            assert_eq!(
                values[1],
                &if id == 17 {
                    Value::Bytes(vec![17; 8192])
                } else {
                    Value::Null
                }
            );
            if seen.is_empty() {
                // The first row is committed: a callback must run after its physical read closes.
                store
                    .put(1, Document::from([("value".into(), Value::Int(-1))]))
                    .unwrap();
                store.delete(17).unwrap();
            }
            seen.push(id);
            true
        },
    )
    .unwrap();
    assert_eq!(seen, ids);
    assert_eq!(control.memory().used(), 0);
    store.conn.rollback_transaction().unwrap();

    // Sparse metadata requests keep the captured deletions and duplicates after undo.
    let mut seen = Vec::new();
    read.visit_projection(&ids, &[], &mut |id, present, values| {
        assert_eq!(values.len(), 0);
        seen.push((id, present));
        true
    })
    .unwrap();
    assert_eq!(seen, ids.map(|id| (id, !matches!(id, 32 | 97))));
    assert_eq!(control.memory().used(), 0);
    assert!(store.get(1).unwrap().is_none());
    let mut selected = captured.view.selected(&control);
    let row = read
        .projected(&mut selected, 17, &["bytes"])
        .unwrap()
        .unwrap();
    assert_eq!(row.metadata().tuple_xmin(), Some(42));
    assert_eq!(row.fields()["bytes"], Value::Bytes(vec![17; 8192]));
    drop((row, selected));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn requested_projections_stop_before_later_admission_and_release_failed_reads() {
    let mut store = fixture();
    store.conn.begin_transaction().unwrap();
    for (id, body) in [(1, "small".into()), (2, "x".repeat(64 << 10))] {
        store
            .put(id, Document::from([("body".into(), Value::Str(body))]))
            .unwrap();
    }
    let captured = store.conn.native_snapshot().unwrap().unwrap();
    let control = StorageReadControl::with_limit(32 << 10);
    let read = NativeDocumentRead::with_control(&captured, "docs", &control, None).unwrap();
    for ids in [[1, 2], [1, u64::MAX]] {
        let mut calls = 0;
        read.visit_projection(&ids, &["body"], &mut |id, present, values| {
            assert_eq!(id, 1);
            assert!(present);
            assert_eq!(values, [&Value::Str("small".into())]);
            calls += 1;
            false
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(control.memory().used(), 0);
    }
    let mut calls = 0;
    let failed = read.visit_projection(&[1, 2], &["body"], &mut |_, _, _| {
        calls += 1;
        true
    });
    assert!(matches!(failed, Err(SQLiteError::Memory(_))));
    assert_eq!(calls, 1);
    assert_eq!(control.memory().used(), 0);

    let cancelled = read.visit_projection(&[1, 2], &["body"], &mut |_, _, _| {
        control.cancellation().cancel();
        false
    });
    assert!(matches!(cancelled, Err(SQLiteError::Cancelled(_))));
    assert_eq!(control.memory().used(), 0);
    control.cancellation().reset();
    store.conn.rollback_transaction().unwrap();
}
