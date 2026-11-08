//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    document_store::{image::decode_document_image, Document},
    DocumentMetadata, DocumentStore,
};

fn input(control: &StorageReadControl, payload: &str, capacity: usize) -> Budgeted<StoredDocument> {
    let mut text = String::with_capacity(capacity);
    text.push_str(payload);
    let mut name = String::with_capacity(64);
    name.push_str("payload");
    let fields = Value::Map(Document::from([
        (name, Value::Str(text)),
        ("xmin".into(), Value::Int(-9)),
    ]));
    let memory = fields
        .reserve_retained_payload(control.memory(), control.cancellation())
        .unwrap();
    let Value::Map(fields) = fields else {
        unreachable!()
    };
    Budgeted::new(
        StoredDocument::with_metadata(fields, DocumentMetadata::with_tuple_xmin(42)),
        memory,
    )
}

fn text_pointer(document: &StoredDocument) -> *const u8 {
    let Value::Str(text) = &document.fields()["payload"] else {
        panic!("text payload")
    };
    text.as_ptr()
}

#[test]
fn writable_document_images_preserve_exact_input_and_native_mutations() {
    let control = StorageReadControl::with_limit(1 << 20);
    // Independent UQADOC01/UQAVAL01 fixture: private xmin 42, one map field, signed integer -9.
    let mut fixture = b"UQADOC01\x01\x2a\0\0\0UQAVAL01\x11".to_vec();
    fixture.extend_from_slice(&1_u64.to_le_bytes());
    fixture.extend_from_slice(&4_u64.to_le_bytes());
    fixture.extend_from_slice(b"xmin");
    fixture.push(3);
    fixture.extend_from_slice(&(-9_i64).to_le_bytes());
    let decoded = decode_document_image(&fixture, &control).unwrap();
    let first = input(&control, "original", 8192);
    let pointer = text_pointer(&first);
    let name = first.fields().keys().next().unwrap().as_ptr();
    let mut store = MemoryDocumentStoreBuilder::new(&control)
        .add_document(u64::MAX, decoded)
        .unwrap()
        .add_document(0, first)
        .unwrap()
        .add_document(7, input(&control, "other", 1024))
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(store.doc_ids().unwrap(), [0, 7, u64::MAX]);
    assert_eq!(store.next_doc_ids(Some(0), 3).unwrap(), [7, u64::MAX]);
    assert_eq!(store.next_doc_id(Some(u64::MAX)).unwrap(), None);
    assert!(store.next_doc_ids(None, 0).unwrap().is_empty());
    let row = &store.state.documents[&0];
    assert_eq!(store.state.layouts[row.layout_id][0].as_ptr(), name);
    store
        .for_each_fields_multi_ref(&[0], &["payload"], &mut |_, values| {
            let Value::Str(text) = values[0] else {
                panic!("text")
            };
            assert_eq!(text.as_ptr(), pointer);
            assert_eq!(text.capacity(), 8192);
            true
        })
        .unwrap();
    assert_eq!(
        store.get_metadata(u64::MAX).unwrap(),
        Some(DocumentMetadata::with_tuple_xmin(42))
    );
    assert_eq!(
        store.get_field(u64::MAX, "xmin").unwrap(),
        Some(Value::Int(-9))
    );
    assert_eq!(
        store.state.documents[&0].layout_id,
        store.state.documents[&7].layout_id
    );
    let original = store.snapshot().unwrap();
    store
        .put(0, Document::from([("later".into(), Value::Bool(true))]))
        .unwrap();
    assert_eq!(
        store.get_metadata(0).unwrap(),
        Some(DocumentMetadata::with_tuple_xmin(42))
    );
    store
        .patch_fields(0, &Document::from([("added".into(), Value::Int(11))]))
        .unwrap();
    store.delete(7).unwrap();
    assert_eq!(store.doc_ids().unwrap(), [0, u64::MAX]);
    assert_eq!(
        original.get_field(0, "payload").unwrap(),
        Some(Value::Str("original".into()))
    );
    assert!(original.contains_doc_id(7).unwrap());
    drop(original);
    store.clear().unwrap();
    drop(store);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn shared_document_rows_retain_original_payload_after_all_store_roots_retire() {
    let control = StorageReadControl::with_limit(1 << 20);
    let document = input(&control, "kept", 32 << 10);
    let pointer = text_pointer(&document);
    let mut store = MemoryDocumentStoreBuilder::new(&control)
        .add_document(3, document)
        .unwrap()
        .finish()
        .unwrap();
    let mut rows = store
        .get_shared_fields(&[3, 999, 3], &["xmin", "payload", "missing", "payload"])
        .unwrap()
        .unwrap();
    assert!(rows[1].is_none());
    let first = rows[0].take().unwrap();
    let second = rows[2].take().unwrap();
    store.clear().unwrap();
    drop(store);
    assert!(control.memory().used() >= 32 << 10);
    first.with_projected(|values| {
        assert_eq!(*values[0], Value::Int(-9));
        assert_eq!(*values[2], Value::Null);
        for slot in [1, 3] {
            let Value::Str(text) = values[slot] else {
                panic!("text")
            };
            assert_eq!(text.as_ptr(), pointer);
        }
    });
    let (values, projection) = first.into_shared_parts();
    assert!(matches!(values, SharedDocumentValues::Retained(_)));
    drop(second);
    drop(projection);
    assert!(control.memory().used() >= 32 << 10);
    drop(values);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn writable_document_snapshots_keep_image_names_and_values_through_copy_on_write() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut source = MemoryDocumentStoreBuilder::new(&control)
        .add_document(1, input(&control, "before", 8192))
        .unwrap()
        .finish()
        .unwrap();
    let mut writable = source.writable_snapshot().unwrap();
    let independent = source.clone();
    let capture = StorageReadControl::with_limit(1 << 20);
    let fixed = source.snapshot_with_control(&capture).unwrap();
    source.clear().unwrap();
    drop(source);
    writable
        .put(1, Document::from([("after".into(), Value::Int(9))]))
        .unwrap();
    assert_eq!(
        independent.get_field(1, "payload").unwrap(),
        Some(Value::Str("before".into()))
    );
    assert_eq!(
        fixed.get_field(1, "payload").unwrap(),
        Some(Value::Str("before".into()))
    );
    assert!(control.memory().used() >= 8192);
    drop(independent);
    drop(fixed);
    assert_eq!(capture.memory().used(), 0);
    assert!(control.memory().used() > 0);
    writable.clear().unwrap();
    drop(writable);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_document_publication_rejects_partial_foreign_duplicate_and_cancelled_corpora() {
    for kind in [
        "foreign",
        "short",
        "duplicate",
        "cancel",
        "allocation",
        "header",
        "unwind",
    ] {
        let control = StorageReadControl::with_limit(32 << 10);
        let foreign = StorageReadControl::with_limit(32 << 10);
        let builder = MemoryDocumentStoreBuilder::new(&control)
            .add_document(1, input(&control, "first", 4096))
            .unwrap();
        let document = if kind == "foreign" {
            input(&foreign, "next", 4096)
        } else if kind == "short" {
            Budgeted::new(
                StoredDocument::new(Document::from([(
                    "field".into(),
                    Value::Str("unreserved".into()),
                )])),
                control.memory().empty_reservation(),
            )
        } else {
            input(&control, "next", 4096)
        };
        if kind == "cancel" {
            control.cancellation().cancel();
        }
        let blocker = matches!(kind, "allocation" | "header").then(|| {
            control
                .memory()
                .reserve(control.memory().available())
                .unwrap()
        });
        match kind {
            "header" => {
                let result = builder.finish();
                drop(document);
                assert!(matches!(result, Err(StorageBackendError::Memory(_))));
            }
            "unwind" => {
                assert!(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _builder = builder.add_document(2, document).unwrap();
                        panic!("discard unpublished corpus");
                    }))
                    .is_err()
                );
            }
            _ => {
                assert!(builder
                    .add_document(if kind == "duplicate" { 1 } else { 2 }, document)
                    .is_err());
            }
        }
        assert_eq!(
            control.memory().used(),
            blocker.as_ref().map_or(0, MemoryReservation::bytes)
        );
        drop(blocker);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(foreign.memory().used(), 0);
    }
}

#[test]
fn completed_document_import_is_writable_after_input_cancellation_and_legacy_export() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut store = MemoryDocumentStoreBuilder::new(&control)
        .add_document(1, input(&control, "original", 8192))
        .unwrap()
        .finish()
        .unwrap();
    let row = store
        .get_shared_fields(&[1], &["payload"])
        .unwrap()
        .unwrap()
        .pop()
        .unwrap()
        .unwrap();
    let (values, _) = row.into_parts();
    control.cancellation().cancel();
    assert_eq!(values[0], Value::Str("original".into()));
    store
        .put(2, Document::from([("native".into(), Value::Int(19))]))
        .unwrap();
    assert_eq!(store.get_field(2, "native").unwrap(), Some(Value::Int(19)));
    let fresh = StorageReadControl::with_limit(1 << 20);
    let mut bytes = Vec::new();
    store
        .write_image_document(1, &mut bytes, &fresh)
        .unwrap()
        .unwrap();
    assert_eq!(
        decode_document_image(&bytes, &fresh).unwrap().fields()["payload"],
        Value::Str("original".into())
    );
    drop(store);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(values[0], Value::Str("original".into()));
    assert_eq!(fresh.memory().used(), 0);
}

#[test]
fn native_document_identity_collection_allocates_only_its_complete_output_buffer() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut ordinary = MemoryDocumentStore::new();
    let mut restored = MemoryDocumentStoreBuilder::new(&control);
    for id in 0..1024 {
        ordinary.put(id, Document::new()).unwrap();
        restored = restored
            .add_document(
                id,
                Budgeted::new(
                    StoredDocument::default(),
                    control.memory().empty_reservation(),
                ),
            )
            .unwrap();
    }
    let restored = restored.finish().unwrap();
    for store in [&ordinary, &restored] {
        let mut ids = None;
        let allocation = allocation_counter::measure(|| ids = Some(store.doc_ids().unwrap()));
        assert_eq!(ids.unwrap(), (0..1024).collect::<Vec<_>>());
        assert_eq!(
            allocation.bytes_total,
            1024 * size_of::<DocId>() as u64,
            "{allocation:?}"
        );
        let mut rows = store.state.documents.iter();
        assert_eq!(rows.len(), 1024);
        for remaining in (0..1024).rev() {
            assert!(rows.next().is_some());
            assert_eq!(rows.size_hint(), (remaining, Some(remaining)));
        }
        assert!(rows.next().is_none());
    }
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}
