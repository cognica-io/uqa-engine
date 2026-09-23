//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    key_value::{KeyValueDocumentStore, KeyValueStore, MemoryKeyValueStore},
    DocumentStore, StorageBackendError,
};
use std::sync::Arc;
use uqa_core::Value;

#[test]
fn identity_pages_share_the_fixed_read_allowance_and_preserve_the_selected_view() {
    let store = Arc::new(MemoryKeyValueStore::new());
    let mut documents = KeyValueDocumentStore::new(store.clone(), "docs");
    for id in [1, 3] {
        documents
            .put(id, [("value".into(), Value::Int(7))].into())
            .unwrap();
    }
    let snapshot = documents.snapshot().unwrap();
    let mut control = None;
    store
        .with_read_view(&mut |read| {
            control = Some(read.control().clone());
            Ok(())
        })
        .unwrap();
    let control = control.unwrap();
    let retained = control.memory().used();
    let full = control
        .memory()
        .reserve(control.memory().limit() - retained)
        .unwrap();
    for view in [&documents as &dyn DocumentStore, snapshot.as_ref()] {
        assert!(matches!(
            view.next_doc_ids(None, 2),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), control.memory().limit());
        assert!(view.next_doc_ids(None, 0).unwrap().is_empty());
    }
    drop(full);
    assert_eq!(control.memory().used(), retained);
    documents.delete(1).unwrap();
    assert_eq!(documents.next_doc_ids(None, 2).unwrap(), [3]);
    assert_eq!(snapshot.next_doc_ids(None, 2).unwrap(), [1, 3]);
    assert_eq!(snapshot.next_doc_ids(Some(1), 1).unwrap(), [3]);
    assert_eq!(control.memory().used(), retained);
    control.cancellation().cancel();
    assert!(matches!(
        snapshot.next_doc_ids(None, 0),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(snapshot);
    assert_eq!(control.memory().used(), 0);
}
