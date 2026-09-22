//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{catalog::Catalog, document_store::SQLiteDocumentStore, ManagedConnection};
use uqa_core::Value;
use uqa_storage::{
    document_store::Document, mvcc::VersionedSessionOptions, DocumentMetadata, DocumentStore,
    StorageBackendError, StoredDocument,
};

fn fixture() -> SQLiteDocumentStore {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let mut store = SQLiteDocumentStore::new(connection, "documents");
    for id in [1, 2] {
        store
            .put_stored(
                id,
                StoredDocument::with_metadata(
                    Document::from([
                        ("body".into(), Value::Str("body".repeat(2048))),
                        (
                            "bytes".into(),
                            Value::Bytes(vec![u8::try_from(id).unwrap(); 16384]),
                        ),
                        (
                            "array".into(),
                            Value::List(vec![Value::Int(1), Value::Int(2)]),
                        ),
                    ]),
                    DocumentMetadata::with_tuple_xmin(42),
                ),
            )
            .unwrap();
    }
    store
}

#[test]
fn native_borrowed_projection_keeps_decoded_payloads_during_reentrant_writes() {
    let mut store = fixture();
    let control = store.conn.retention_control().unwrap();
    let snapshot = store.snapshot().unwrap();
    let baseline = control.memory().used();
    let mut calls = Vec::new();
    snapshot
        .for_each_fields_multi_ref_with_presence(
            &[2, 99, 1, 2],
            &["bytes", "body", "bytes"],
            &mut |id, present, values| {
                assert_eq!(present, id != 99);
                assert!(std::ptr::eq(values[0], values[2]));
                if present {
                    assert!(control.memory().used() >= baseline + 16384 + 8192);
                    assert_eq!(
                        values[0],
                        &Value::Bytes(vec![u8::try_from(id).unwrap(); 16384])
                    );
                    assert_eq!(values[1], &Value::Str("body".repeat(2048)));
                }
                if calls.is_empty() {
                    store
                        .put(1, Document::from([("bytes".into(), Value::Bytes(vec![9]))]))
                        .unwrap();
                }
                calls.push(id);
                true
            },
        )
        .unwrap();
    assert_eq!(calls, [2, 99, 1, 2]);
    assert_eq!(
        snapshot.get_metadata(1).unwrap().unwrap().tuple_xmin(),
        Some(42)
    );
    assert_eq!(
        store.get_field(1, "bytes").unwrap(),
        Some(Value::Bytes(vec![9]))
    );
}

#[test]
fn native_decode_quota_and_cancel_errors_preserve_the_selected_rows() {
    let store = fixture();
    let control = store.conn.retention_control().unwrap();
    let snapshot = store.snapshot().unwrap();
    let baseline = control.memory().used();
    let hold = control
        .memory()
        .reserve(control.memory().limit() - baseline)
        .unwrap();
    assert!(matches!(
        snapshot.get_stored(1),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(hold);
    assert_eq!(control.memory().used(), baseline);
    let mut calls = 0;
    let result = snapshot.for_each_fields_multi_ref(&[1, 2], &["bytes"], &mut |_, _| {
        calls += 1;
        control.cancellation().cancel();
        true
    });
    assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
    assert_eq!(calls, 1);
    assert_eq!(control.memory().used(), baseline);
    control.cancellation().reset();
    assert_eq!(
        snapshot.get_field(1, "array").unwrap(),
        Some(Value::List(vec![Value::Int(1), Value::Int(2)]))
    );
    assert_eq!(control.memory().used(), baseline);
}
