//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{RetainedDocumentFields, RetainedStoredDocument};

fn retained(control: &StorageReadControl) -> RetainedStoredDocument {
    let mut text = String::with_capacity(8192);
    text.push_str("payload");
    let fields = RetainedDocumentFields::new(
        Arc::new([("payload".into(), Value::Str(text))].into()),
        control,
    )
    .unwrap();
    RetainedStoredDocument::with_metadata(fields, DocumentMetadata::with_tuple_xmin(42))
}

#[test]
fn retained_adoption_preserves_unique_payload_addresses_and_the_tuple_lease() {
    let control = StorageReadControl::with_limit(11_000);
    let row = retained(&control);
    let Value::Str(text) = &row.fields()["payload"] else {
        panic!("text payload");
    };
    let address = text.as_ptr();
    let mut builder = RetainedDocumentStoreBuilder::new(&control);
    builder.add_retained_document(7, row).unwrap();
    let store = builder.finish().unwrap();
    let nested = store.snapshot().unwrap();
    drop(store);
    assert!(control.memory().used() >= 8192);
    nested
        .for_each_fields_multi_ref(&[7], &["payload"], &mut |_, values| {
            let Value::Str(text) = values[0] else {
                panic!("text payload");
            };
            assert_eq!(text.as_ptr(), address);
            assert_eq!(text.capacity(), 8192);
            true
        })
        .unwrap();
    assert_eq!(
        nested.get_metadata(7).unwrap(),
        Some(DocumentMetadata::with_tuple_xmin(42))
    );
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn shared_adoption_copies_once_and_preserves_the_other_readers_reservation() {
    let control = StorageReadControl::with_limit(16 << 10);
    let row = retained(&control);
    let used = control.memory().used();
    let mut builder = RetainedDocumentStoreBuilder::new(&control);
    builder.add_retained_document(7, row.clone()).unwrap();
    let store = builder.finish().unwrap();
    assert!(control.memory().used() > used);
    assert_eq!(store.get_stored(7).unwrap().unwrap().fields(), row.fields());
    assert_eq!(store.get_metadata(7).unwrap(), Some(row.metadata()));
    drop(store);
    assert_eq!(control.memory().used(), used);
    drop(row);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn budgeted_adoption_keeps_input_charged_while_admitting_corpus_workspace() {
    let control = StorageReadControl::with_limit(16 << 10);
    let mut builder = RetainedDocumentStoreBuilder::new(&control);
    let row = retained(&control).into_budgeted(&control).unwrap();
    let blocked = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    assert!(matches!(
        builder.add_budgeted_document(7, row),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(control.memory().used(), blocked.bytes());
    drop(blocked);
    let store = builder.finish().unwrap();
    assert!(store.is_empty().unwrap());
    drop(store);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn budgeted_adoption_rejects_foreign_incomplete_and_cancelled_inputs() {
    for kind in ["foreign", "incomplete", "cancelled"] {
        let control = StorageReadControl::with_limit(16 << 10);
        let foreign = StorageReadControl::with_limit(16 << 10);
        let mut builder = RetainedDocumentStoreBuilder::new(&control);
        builder.add_document(1, row(Value::Int(11), 41)).unwrap();
        let original = control.memory().used();
        let input = match kind {
            "foreign" => retained(&foreign).into_budgeted(&foreign).unwrap(),
            "incomplete" => Budgeted::new(
                row(Value::Str("unreserved".into()), 42),
                control.memory().empty_reservation(),
            ),
            "cancelled" => {
                let row = retained(&control).into_budgeted(&control).unwrap();
                control.cancellation().cancel();
                row
            }
            _ => unreachable!(),
        };
        let error = builder.add_budgeted_document(2, input).unwrap_err();
        if kind == "cancelled" {
            assert!(matches!(error, StorageBackendError::Cancelled(_)));
        } else {
            assert!(matches!(error, StorageBackendError::Other(_)));
        }
        assert_eq!(control.memory().used(), original);
        assert_eq!(foreign.memory().used(), 0);
        if kind == "cancelled" {
            drop(builder);
        } else {
            let store = builder.finish().unwrap();
            assert_eq!(store.doc_ids().unwrap(), [1]);
            assert_eq!(store.get_field(1, "payload").unwrap(), Some(Value::Int(11)));
            drop(store);
        }
        assert_eq!(control.memory().used(), 0);
    }
}
