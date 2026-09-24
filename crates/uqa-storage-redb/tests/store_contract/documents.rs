//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document ownership and durable reservation transfer use redb's real common consumers.

use super::*;
use uqa_storage::key_value::conformance::{verify_document_ownership, verify_document_reopen};
use uqa_storage::{
    document_store::{read_document_ids, read_field_presence},
    key_value::KeyValueDocumentStore,
    read_control::StorageReadControl,
    DocumentStore, StorageBackendError,
};

#[test]
fn document_owners_coordinate_and_reopen_after_all_handles_close() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("documents.redb");
    {
        let storage = RedbStorage::open(&path).unwrap();
        let a: std::sync::Arc<dyn KeyValueStore> = std::sync::Arc::new(storage.store());
        let b = a.open_session().unwrap();
        verify_document_ownership(&a, &b).unwrap();
    }
    let reopened = RedbStorage::open(&path).unwrap();
    let store: std::sync::Arc<dyn KeyValueStore> = std::sync::Arc::new(reopened.store());
    verify_document_reopen(&store).unwrap();
}

#[test]
fn controlled_identity_pages_keep_redb_snapshots_and_the_invoking_allowance() {
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("identity-pages.redb")).unwrap();
    let mut documents = KeyValueDocumentStore::new(std::sync::Arc::new(storage.store()), "docs");
    for id in [1, 3, 5] {
        documents
            .put(
                id,
                [("opaque".into(), uqa_core::Value::Str("x".repeat(128 << 10)))].into(),
            )
            .unwrap();
    }
    let snapshot = documents.snapshot().unwrap();
    documents.delete(3).unwrap();
    documents.put(4, std::collections::BTreeMap::new()).unwrap();
    let control = StorageReadControl::with_limit(8 << 10);
    for (view, expected) in [
        (&documents as &dyn DocumentStore, [4, 5]),
        (snapshot.as_ref(), [3, 5]),
    ] {
        let ids = read_document_ids(view, Some(1), 2, &control).unwrap();
        assert_eq!(&*ids, &expected);
        assert_eq!(control.memory().used(), ids.capacity() * size_of::<u64>());
        let occupied = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used())
            .unwrap();
        assert!(matches!(
            read_document_ids(view, None, 1, &control),
            Err(StorageBackendError::Memory(_))
        ));
        assert!(read_document_ids(view, None, 0, &control)
            .unwrap()
            .is_empty());
        drop(occupied);
        drop(ids);
        assert_eq!(control.memory().used(), 0);
        control.cancellation().cancel();
        assert!(matches!(
            read_document_ids(view, None, 0, &control),
            Err(StorageBackendError::Cancelled(_))
        ));
        control.cancellation().reset();
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn controlled_whole_rows_keep_redb_selected_payloads_after_provider_close() {
    use uqa_storage::{document_store::read_stored_documents, DocumentMetadata, StoredDocument};
    let directory = tempfile::tempdir().unwrap();
    let storage = RedbStorage::open(directory.path().join("whole-rows.redb")).unwrap();
    let mut documents = KeyValueDocumentStore::new(std::sync::Arc::new(storage.store()), "docs");
    let expected = StoredDocument::with_metadata(
        [
            ("payload".into(), uqa_core::Value::Bytes(vec![7; 32 << 10])),
            (
                "record".into(),
                uqa_core::Value::Record(vec![("field".into(), uqa_core::Value::Int(9))]),
            ),
        ]
        .into(),
        DocumentMetadata::with_tuple_xmin(87),
    );
    documents.put_stored(3, expected.clone()).unwrap();
    let snapshot = documents.snapshot().unwrap();
    documents.delete(3).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let presence = read_field_presence(
        snapshot.as_ref(),
        &[3, 99, 3],
        &["record", "missing", "payload"],
        &control,
    )
    .unwrap();
    assert_eq!(
        &*presence,
        &[true, false, true, false, false, false, true, false, true]
    );
    assert!(control.memory().used() < 32 << 10);
    drop(presence);
    assert_eq!(control.memory().used(), 0);
    let page = read_stored_documents(snapshot.as_ref(), &[3, 99, 3], &control).unwrap();
    assert!(page[1].is_none());
    for index in [0, 2] {
        assert_eq!(page[index].as_ref().unwrap().fields(), expected.fields());
        assert_eq!(
            page[index].as_ref().unwrap().metadata(),
            expected.metadata()
        );
    }
    assert!(control.memory().used() >= 64 << 10);
    assert!(read_stored_documents(&documents, &[3], &control).unwrap()[0].is_none());
    let tiny = StorageReadControl::with_limit(4096);
    assert!(matches!(
        read_field_presence(snapshot.as_ref(), &[3], &["payload"], &tiny),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(tiny.memory().used(), 0);
    assert!(matches!(
        read_stored_documents(snapshot.as_ref(), &[3], &tiny),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(tiny.memory().used(), 0);
    tiny.cancellation().cancel();
    assert!(matches!(
        read_field_presence(snapshot.as_ref(), &[3], &["payload"], &tiny),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        read_stored_documents(snapshot.as_ref(), &[3], &tiny),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(snapshot);
    drop(documents);
    drop(storage);
    assert_eq!(page[0].as_ref().unwrap().fields(), expected.fields());
    drop(page);
    assert_eq!(control.memory().used(), 0);
}
