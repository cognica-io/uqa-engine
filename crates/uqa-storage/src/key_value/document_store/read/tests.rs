//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    key_value::{KeyValueDocumentStore, KeyValueStore, MemoryKeyValueStore},
    DocumentStore, StorageBackendError,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::Value;

#[test]
fn borrowed_identity_pages_retain_the_allowance_and_allow_reentrant_writes() {
    let store = Arc::new(MemoryKeyValueStore::new());
    let mut documents = KeyValueDocumentStore::new(store.clone(), "docs");
    for id in [1, 3, 5] {
        documents.put(id, BTreeMap::new()).unwrap();
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
    assert_eq!(
        snapshot
            .for_each_next_fields(None, 3, &["value"], &mut |_, _| {
                panic!("an unsupported cursor must not invoke its consumer")
            })
            .unwrap(),
        None
    );
    let mut visited = Vec::new();
    assert_eq!(
        snapshot
            .for_each_next_fields(None, 3, &[], &mut |id, values| {
                assert!(values.is_empty());
                assert!(
                    control.memory().used() > retained,
                    "provider IDs must remain charged through the callback"
                );
                visited.push(id);
                if id == 1 {
                    documents.delete(3).unwrap();
                    documents.put(4, BTreeMap::new()).unwrap();
                }
                true
            })
            .unwrap(),
        Some(3)
    );
    assert_eq!(visited, [1, 3, 5]);
    assert_eq!(documents.next_doc_ids(None, 3).unwrap(), [1, 4, 5]);
    assert_eq!(control.memory().used(), retained);
    let mut stopped = Vec::new();
    assert_eq!(
        snapshot
            .for_each_next_fields(Some(1), 3, &[], &mut |id, _| {
                stopped.push(id);
                false
            })
            .unwrap(),
        Some(1)
    );
    assert_eq!(stopped, [3]);
    assert_eq!(control.memory().used(), retained);
    let full = control
        .memory()
        .reserve(control.memory().limit() - retained)
        .unwrap();
    assert!(matches!(
        snapshot.for_each_next_fields(None, 1, &[], &mut |_, _| {
            panic!("a rejected page must not invoke its consumer")
        }),
        Err(StorageBackendError::Memory(_))
    ));
    drop(full);
    assert_eq!(control.memory().used(), retained);
    assert_eq!(
        snapshot
            .for_each_next_fields(None, 0, &[], &mut |_, _| {
                panic!("an empty page must not invoke its consumer")
            })
            .unwrap(),
        Some(0)
    );
    control.cancellation().cancel();
    assert!(matches!(
        snapshot.for_each_next_fields(None, 0, &[], &mut |_, _| true),
        Err(StorageBackendError::Cancelled(_))
    ));
}

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
