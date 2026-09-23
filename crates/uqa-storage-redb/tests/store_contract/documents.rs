//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document ownership and durable reservation transfer use redb's real common consumers.

use super::*;
use uqa_storage::key_value::conformance::{verify_document_ownership, verify_document_reopen};
use uqa_storage::{
    document_store::read_document_ids, key_value::KeyValueDocumentStore,
    read_control::StorageReadControl, DocumentStore, StorageBackendError,
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
