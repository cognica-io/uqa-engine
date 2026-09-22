//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    document_store::Document,
    key_value::{codec::document_key, KeyValueStore, MemoryKeyValueStore},
    read_control::StorageReadControl,
    DocumentStore, StorageBackendError,
};
use std::sync::Arc;

fn fixture() -> (
    KeyValueDocumentStore,
    Arc<MemoryKeyValueStore>,
    StorageReadControl,
) {
    let store = Arc::new(MemoryKeyValueStore::new());
    let mut documents = KeyValueDocumentStore::new(store.clone(), "docs");
    for (id, text) in [(1, "first"), (2, "second")] {
        documents
            .put(
                id,
                Document::from([("body".into(), Value::Str(text.repeat(4096)))]),
            )
            .unwrap();
    }
    let mut control = None;
    store
        .with_read_view(&mut |read| {
            control = Some(read.control().clone());
            Ok(())
        })
        .unwrap();
    (documents, store, control.unwrap())
}

#[test]
fn borrowed_rows_keep_one_payload_per_identity_through_reentrant_callbacks() {
    let (documents, _, control) = fixture();
    let mut writer = documents.clone();
    let unrelated = control.memory().reserve(7).unwrap();
    let mut calls = Vec::new();
    let mut second_address = None;
    documents
        .for_each_fields_multi_ref_with_presence(
            &[2, 99, 1, 2],
            &["body", "missing", "body"],
            &mut |id, present, values| {
                assert!(control.memory().used() >= 7 + (5 + 6) * 4096);
                assert!(std::ptr::eq(values[0], values[2]));
                assert_eq!(values[1], &Value::Null);
                assert_eq!(present, id != 99);
                match id {
                    1 => assert_eq!(values[0], &Value::Str("first".repeat(4096))),
                    2 => {
                        assert_eq!(values[0], &Value::Str("second".repeat(4096)));
                        let address = std::ptr::from_ref(values[0]);
                        if let Some(previous) = second_address {
                            assert_eq!(previous, address);
                        }
                        second_address = Some(address);
                    }
                    _ => assert_eq!(values[0], &Value::Null),
                }
                if calls.is_empty() {
                    writer
                        .put(
                            1,
                            Document::from([("body".into(), Value::Str("changed".into()))]),
                        )
                        .unwrap();
                }
                calls.push(id);
                true
            },
        )
        .unwrap();
    assert_eq!(calls, [2, 99, 1, 2]);
    assert_eq!(control.memory().used(), 7);
    assert_eq!(
        documents.get_field(1, "body").unwrap(),
        Some(Value::Str("changed".into()))
    );
    drop(unrelated);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn quota_and_cancellation_release_only_the_failed_projection() {
    let (documents, _, control) = fixture();
    let unrelated = control.memory().reserve(7).unwrap();
    let full = control
        .memory()
        .reserve(control.memory().limit() - 7)
        .unwrap();
    let mut calls = 0;
    let result = documents.for_each_fields_multi_ref(&[1], &["body"], &mut |_, _| {
        calls += 1;
        true
    });
    assert!(matches!(result, Err(StorageBackendError::Memory(_))));
    assert_eq!(calls, 0);
    assert_eq!(control.memory().used(), control.memory().limit());
    drop(full);
    let result = documents.for_each_fields_multi_ref(&[1, 2], &["body"], &mut |_, _| {
        calls += 1;
        control.cancellation().cancel();
        true
    });
    assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
    assert_eq!(calls, 1);
    assert_eq!(control.memory().used(), 7);
    control.cancellation().reset();
    documents
        .for_each_fields_multi(&[2, 1], &["body"], &mut |id, values| {
            calls += 1;
            assert_eq!(id, 2);
            assert_eq!(values, [Value::Str("second".repeat(4096))]);
            false
        })
        .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(control.memory().used(), 7);
    drop(unrelated);
}

#[test]
fn empty_projections_probe_presence_without_decoding_and_snapshots_forward_leases() {
    let (documents, store, control) = fixture();
    store
        .put(&document_key("docs", 3).unwrap(), b"invalid JSON")
        .unwrap();
    let mut calls = Vec::new();
    documents
        .for_each_fields_multi_ref_with_presence(&[3, 4, 3], &[], &mut |id, present, values| {
            calls.push((id, present));
            assert!(values.is_empty());
            true
        })
        .unwrap();
    assert_eq!(calls, [(3, true), (4, false), (3, true)]);
    assert_eq!(control.memory().used(), 0);
    let snapshot = documents.snapshot().unwrap();
    let nested = snapshot.snapshot().unwrap();
    let retained = control.memory().used();
    nested
        .for_each_fields_multi_ref(&[1], &["body"], &mut |_, values| {
            assert_eq!(values, [&Value::Str("first".repeat(4096))]);
            assert!(control.memory().used() >= retained + 5 * 4096);
            true
        })
        .unwrap();
    assert_eq!(control.memory().used(), retained);
    drop((nested, snapshot));
    assert_eq!(control.memory().used(), 0);
}
