//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn row(value: Value, xmin: u32) -> StoredDocument {
    StoredDocument::with_metadata(
        BTreeMap::from([("payload".into(), value)]),
        DocumentMetadata::with_tuple_xmin(xmin),
    )
}

#[test]
fn adopted_buffers_and_metadata_live_until_the_last_nested_reader() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut text = String::with_capacity(4096);
    text.push_str("original");
    let address = text.as_ptr();
    let mut builder = RetainedDocumentStoreBuilder::new(&control);
    builder.add_document(9, row(Value::Str(text), 41)).unwrap();
    builder.add_document(0, row(Value::Null, 42)).unwrap();
    let store = builder.finish().unwrap();
    let used = control.memory().used();
    assert!(used >= 4096);
    let nested = store.snapshot().unwrap().snapshot().unwrap();
    assert_eq!(control.memory().used(), used);
    drop(store);
    assert_eq!(control.memory().used(), used);
    nested
        .for_each_fields_multi_ref(&[9], &["payload"], &mut |_, values| {
            let Value::Str(text) = values[0] else {
                panic!("original text");
            };
            assert_eq!(text.as_ptr(), address);
            assert_eq!(text.capacity(), 4096);
            true
        })
        .unwrap();
    assert_eq!(nested.doc_ids().unwrap(), [0, 9]);
    assert_eq!(
        nested.get_metadata(9).unwrap(),
        Some(DocumentMetadata::with_tuple_xmin(41))
    );
    assert_eq!(
        nested.get_field(9, "payload").unwrap(),
        Some(Value::Str("original".into()))
    );
    assert_eq!(control.memory().used(), used);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn rejected_adoption_preserves_earlier_rows_and_releases_partial_payload_charges() {
    let control = StorageReadControl::with_limit(4096);
    let mut builder = RetainedDocumentStoreBuilder::new(&control);
    builder.add_document(1, row(Value::Int(11), 41)).unwrap();
    let used = control.memory().used();
    let oversized = row(
        Value::Map(BTreeMap::from([
            ("first".into(), Value::Str("x".repeat(512))),
            ("second".into(), Value::Bytes(vec![1; 8192])),
        ])),
        42,
    );
    assert!(matches!(
        builder.add_document(2, oversized),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), used);
    let store = builder.finish().unwrap();
    assert_eq!(store.doc_ids().unwrap(), [1]);
    assert_eq!(store.get_field(1, "payload").unwrap(), Some(Value::Int(11)));
    drop(store);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn finalization_rejects_duplicate_identities_and_reserves_its_shared_header() {
    for duplicate in [false, true] {
        let control = StorageReadControl::with_limit(4096);
        let mut builder = RetainedDocumentStoreBuilder::new(&control);
        builder.add_document(1, row(Value::Int(1), 41)).unwrap();
        if duplicate {
            builder.add_document(1, row(Value::Int(2), 42)).unwrap();
            assert!(matches!(
                builder.finish(),
                Err(StorageBackendError::Other(_))
            ));
        } else {
            let blocked = control
                .memory()
                .reserve(control.memory().limit() - control.memory().used())
                .unwrap();
            assert!(matches!(
                builder.finish(),
                Err(StorageBackendError::Memory(_))
            ));
            assert_eq!(control.memory().used(), blocked.bytes());
            drop(blocked);
        }
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn projections_preserve_order_duplicates_presence_reentry_and_early_stop() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let mut builder = RetainedDocumentStoreBuilder::new(&control);
    builder.add_document(7, row(Value::Int(70), 41)).unwrap();
    builder
        .add_document(2, StoredDocument::new(Document::new()))
        .unwrap();
    let store = builder.finish().unwrap();
    let used = control.memory().used();
    let mut seen = Vec::new();
    store
        .for_each_fields_multi_ref_with_presence(
            &[7, 99, 2, 7],
            &["payload", "missing", "payload"],
            &mut |id, present, values| {
                assert!(control.memory().used() >= used + 3 * size_of::<&Value>());
                assert_eq!(values[0], values[2]);
                if present {
                    assert!(store.get_metadata(id).unwrap().is_some());
                }
                seen.push((id, present, values[0].clone()));
                true
            },
        )
        .unwrap();
    assert_eq!(
        seen,
        [
            (7, true, Value::Int(70)),
            (99, false, Value::Null),
            (2, true, Value::Null),
            (7, true, Value::Int(70))
        ]
    );
    assert_eq!(control.memory().used(), used);
    let mut calls = 0;
    store
        .for_each_fields_multi_ref_with_presence(&[99, 2, 7], &[], &mut |id, present, values| {
            assert_eq!((id, present), (99, false));
            assert!(values.is_empty());
            calls += 1;
            false
        })
        .unwrap();
    assert_eq!(calls, 1);
    let count = store
        .for_each_next_fields(Some(2), 3, &["payload"], &mut |id, values| {
            assert_eq!(id, 7);
            assert_eq!(*values[0], Value::Int(70));
            false
        })
        .unwrap();
    assert_eq!(count, Some(1));
}

#[test]
fn projection_quota_and_cancellation_release_scratch_without_losing_the_corpus() {
    let control = StorageReadControl::with_limit(4096);
    let mut builder = RetainedDocumentStoreBuilder::new(&control);
    builder.add_document(1, row(Value::Int(1), 41)).unwrap();
    let store = builder.finish().unwrap();
    let used = control.memory().used();
    let blocked = control
        .memory()
        .reserve(control.memory().limit() - used)
        .unwrap();
    let result = store.for_each_fields_multi_ref(&[1], &["payload"], &mut |_, _| {
        panic!("quota must reject before the callback")
    });
    assert!(matches!(result, Err(StorageBackendError::Memory(_))));
    drop(blocked);
    assert_eq!(control.memory().used(), used);
    let mut calls = 0;
    let result = store.for_each_fields_multi_ref(&[1, 1], &["payload"], &mut |_, _| {
        calls += 1;
        control.cancellation().cancel();
        true
    });
    assert!(matches!(result, Err(StorageBackendError::Cancelled(_))));
    assert_eq!(calls, 1);
    assert_eq!(control.memory().used(), used);
    assert!(matches!(
        store.get_metadata(1),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        store.snapshot(),
        Err(StorageBackendError::Cancelled(_))
    ));
    drop(store);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn cancelled_builder_releases_every_adopted_row() {
    let control = StorageReadControl::with_limit(4096);
    let mut builder = RetainedDocumentStoreBuilder::new(&control);
    builder.add_document(1, row(Value::Int(1), 41)).unwrap();
    control.cancellation().cancel();
    assert!(matches!(
        builder.add_document(2, row(Value::Int(2), 42)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        builder.finish(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn read_only_access_preserves_missing_null_and_identity_boundaries() {
    let control = StorageReadControl::with_limit(4096);
    let mut builder = RetainedDocumentStoreBuilder::new(&control);
    builder
        .add_document(u64::MAX, row(Value::Null, 41))
        .unwrap();
    builder
        .add_document(0, StoredDocument::new(Document::new()))
        .unwrap();
    let mut store = builder.finish().unwrap();
    assert_eq!(
        store.find_doc_id_by_field("payload", &Value::Null).unwrap(),
        Some(u64::MAX)
    );
    assert_eq!(
        store
            .find_doc_id_by_fields(&["payload".into()], &[Value::Null])
            .unwrap(),
        Some(0)
    );
    assert_eq!(store.next_doc_id(Some(u64::MAX)).unwrap(), None);
    assert!(store.next_doc_ids(None, 0).unwrap().is_empty());
    assert_eq!(store.max_doc_id().unwrap(), u64::MAX);
    assert_eq!(store.iter_all().unwrap().count(), 2);
    assert!(store.put(2, Document::new()).is_err());
    assert!(store.put_stored(2, row(Value::Null, 42)).is_err());
    assert!(store.patch_fields(0, &Document::new()).is_err());
    assert!(store.delete(0).is_err());
    assert!(store.clear().is_err());
    assert_eq!(store.len().unwrap(), 2);
}
