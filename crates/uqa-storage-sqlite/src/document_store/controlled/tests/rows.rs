//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    Catalog, ManagedConnection, SQLiteCompressionOptions, SQLiteDocumentStore, SQLiteKeyValueStore,
};
use uqa_core::{ArrayValue, DecimalValue, Value};
use uqa_storage::{
    document_store::{read_field_presence, read_stored_documents},
    key_value::KeyValueDocumentStore,
    mvcc::VersionedSessionOptions,
    read_control::StorageReadControl,
    DocumentMetadata, DocumentStore, StorageBackendError, StoredDocument,
};

#[derive(Clone, Copy)]
enum Provider {
    Legacy,
    Native,
    KeyValue,
}
const PROVIDERS: [Provider; 3] = [Provider::Legacy, Provider::Native, Provider::KeyValue];

mod presence;

fn document() -> StoredDocument {
    StoredDocument::with_metadata(
        [
            ("text".into(), Value::Str("한글🙂".repeat(2048))),
            ("bytes".into(), Value::Bytes(vec![3; 32 << 10])),
            (
                "numeric".into(),
                Value::Decimal(DecimalValue::parse("-12345678901234567890.00300").unwrap()),
            ),
            (
                "array".into(),
                Value::Array(
                    ArrayValue::with_lower_bounds(vec![Value::Int(4), Value::Null], vec![-9])
                        .unwrap(),
                ),
            ),
            (
                "record".into(),
                Value::Record(vec![
                    ("b".into(), Value::Bool(true)),
                    ("a".into(), Value::Json(" {\"x\":1} ".into())),
                ]),
            ),
            (
                "row".into(),
                Value::Row(vec![Value::FixedChar("a  ".into()), Value::Void]),
            ),
            (
                "floats".into(),
                Value::List(
                    (0..40)
                        .map(|value| Value::Float(f64::from(value)))
                        .collect(),
                ),
            ),
            (
                "tensor".into(),
                Value::List(vec![Value::List(vec![Value::Float(0.25); 40]); 2]),
            ),
        ]
        .into(),
        DocumentMetadata::with_tuple_xmin(93),
    )
}

fn store(connection: &ManagedConnection, provider: Provider) -> Box<dyn DocumentStore> {
    match provider {
        Provider::KeyValue => Box::new(KeyValueDocumentStore::new(
            std::sync::Arc::new(SQLiteKeyValueStore::new(connection.clone()).unwrap()),
            "docs",
        )),
        Provider::Legacy | Provider::Native => {
            Catalog::open(connection.clone()).unwrap();
            if matches!(provider, Provider::Native) {
                connection
                    .bind_native_records(VersionedSessionOptions::default())
                    .unwrap();
            }
            Box::new(SQLiteDocumentStore::new(connection.clone(), "docs"))
        }
    }
}

fn verify(connection: &ManagedConnection, provider: Provider) {
    let mut source = store(connection, provider);
    let expected = document();
    for id in [1, 3, 5] {
        source.put_stored(id, expected.clone()).unwrap();
    }
    let snapshot = (!matches!(provider, Provider::Legacy)).then(|| source.snapshot().unwrap());
    source.delete(3).unwrap();
    let changed = StoredDocument::with_metadata(
        [("changed".into(), Value::Bool(true))].into(),
        DocumentMetadata::with_tuple_xmin(94),
    );
    source.put_stored(1, changed.clone()).unwrap();
    let control = StorageReadControl::with_limit(4 << 20);
    let live = read_stored_documents(source.as_ref(), &[3, 99, 1, 5, 1], &control).unwrap();
    assert!(live[0].is_none() && live[1].is_none());
    for index in [2, 4] {
        assert_eq!(live[index].as_ref().unwrap().fields(), changed.fields());
        assert_eq!(live[index].as_ref().unwrap().metadata(), changed.metadata());
    }
    assert_eq!(live[3].as_ref().unwrap().fields(), expected.fields());
    assert_eq!(live[3].as_ref().unwrap().metadata(), expected.metadata());
    drop(live);
    assert_eq!(control.memory().used(), 0);
    if let Some(snapshot) = snapshot {
        let presence = read_field_presence(
            snapshot.as_ref(),
            &[3, 99, 3],
            &["bytes", "numeric", "missing"],
            &control,
        )
        .unwrap();
        assert_eq!(
            &*presence,
            &[true, true, false, false, false, false, true, true, false]
        );
        drop(presence);
        let page = read_stored_documents(snapshot.as_ref(), &[3, 99, 1, 3], &control).unwrap();
        assert!(page[1].is_none());
        for index in [0, 2, 3] {
            assert_eq!(page[index].as_ref().unwrap().fields(), expected.fields());
            assert_eq!(
                page[index].as_ref().unwrap().metadata(),
                expected.metadata()
            );
        }
        let kept = page[0].as_ref().unwrap().clone();
        drop(page);
        drop(snapshot);
        assert!(control.memory().used() >= 32 << 10);
        assert_eq!(kept.fields(), expected.fields());
        drop(kept);
        assert_eq!(control.memory().used(), 0);
    }
    let tiny = StorageReadControl::with_limit(8192);
    assert!(matches!(
        read_stored_documents(source.as_ref(), &[1, 5], &tiny),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(tiny.memory().used(), 0);
    let presence = read_field_presence(
        source.as_ref(),
        &[3, 1, 5],
        &["bytes", "numeric", "missing"],
        &control,
    )
    .unwrap();
    assert_eq!(
        &*presence,
        &[false, false, false, false, false, false, true, true, false]
    );
    drop(presence);
    assert!(read_stored_documents(source.as_ref(), &[], &tiny)
        .unwrap()
        .is_empty());
    control.cancellation().cancel();
    assert!(matches!(
        read_stored_documents(source.as_ref(), &[5], &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
    assert!(matches!(
        read_field_presence(source.as_ref(), &[5], &["bytes"], &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    control.cancellation().reset();
    if let Some(captured) = connection.retention_control() {
        captured.cancellation().cancel();
        assert!(matches!(
            read_stored_documents(source.as_ref(), &[5], &control),
            Err(StorageBackendError::Cancelled(_))
        ));
        captured.cancellation().reset();
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn whole_row_pages_keep_typed_payloads_metadata_and_caller_control_in_every_sqlite_mapping() {
    for provider in PROVIDERS {
        verify(&ManagedConnection::open_in_memory().unwrap(), provider);
    }
}

#[test]
fn whole_row_pages_preserve_all_file_modes_and_retained_native_views() {
    for provider in PROVIDERS {
        for mode in 0..4 {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("whole-rows.sqlite");
            let connection = match mode {
                0 => ManagedConnection::open(&path),
                1 => ManagedConnection::open_encrypted(&path, "whole-row-test"),
                2 => ManagedConnection::open_compressed(&path, SQLiteCompressionOptions::default()),
                _ => ManagedConnection::open_compressed_encrypted(
                    &path,
                    "whole-row-test",
                    SQLiteCompressionOptions::default(),
                ),
            }
            .unwrap();
            verify(&connection, provider);
        }
    }
}

#[test]
fn native_whole_row_owner_addressing_cannot_use_the_captured_allowance_for_caller_output() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let source = SQLiteDocumentStore::new(connection, "x".repeat(16 << 10));
    let control = StorageReadControl::with_limit(128);
    assert!(matches!(
        read_stored_documents(&source, &[1], &control),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), 0);
    assert!(read_stored_documents(&source, &[], &control)
        .unwrap()
        .is_empty());
}
