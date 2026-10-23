//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional document callbacks retain their original view across writes and reopen.

use std::sync::Arc;

use uqa_core::{RecordValue, Value};

use crate::{
    document_store::Document, DocumentMetadata, DocumentStore, KeyValueDocumentStore,
    KeyValueStore, StorageBackendResult, StoredDocument,
};

fn document() -> StoredDocument {
    StoredDocument::with_metadata(
        Document::from([
            ("long-name".repeat(2048), Value::Int(42)),
            ("null".into(), Value::Null),
            (
                "payload".into(),
                Value::Record(RecordValue::from_parts(
                    vec![("bytes".into(), Value::Bytes(vec![17; 4096]))],
                    Some(20_001),
                )),
            ),
        ]),
        DocumentMetadata::with_tuple_xmin(73),
    )
}

fn verify_view(view: &dyn DocumentStore, mut reenter: impl FnMut()) -> StorageBackendResult<()> {
    let expected = document();
    let name = "long-name".repeat(2048);
    let mut visited = Vec::new();
    view.for_each_fields_multi_ref_with_presence(
        &[1, 99, 1],
        &["payload", &name, "missing", "null", "payload"],
        &mut |id, present, values| {
            assert_eq!(present, id == 1);
            if present {
                assert!(values[0].has_same_representation(&expected.fields()["payload"]));
                assert_eq!(values[1], &Value::Int(42));
                assert!(std::ptr::eq(values[0], values[4]));
            } else {
                assert!(values.iter().all(|value| **value == Value::Null));
            }
            assert_eq!(values[2..4], [&Value::Null, &Value::Null]);
            if visited.is_empty() {
                reenter();
            }
            visited.push(id);
            true
        },
    )?;
    assert_eq!(visited, [1, 99, 1]);
    assert_eq!(view.get_metadata(1)?.unwrap().tuple_xmin(), Some(73));
    Ok(())
}

/// Exercise a fresh provider's committed/private snapshots and callbacks that
/// reenter persistence. Leave a committed record for closed-file verification.
pub fn verify_document_projections(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let mut documents = KeyValueDocumentStore::new(store.clone(), "projection_docs");
    documents.put_stored(1, document())?;
    let committed = documents.snapshot()?;
    store.begin_transaction()?;
    documents.put(2, Document::from([("private".into(), Value::Int(2))]))?;
    let private = documents.snapshot()?;
    store.rollback_transaction()?;
    verify_view(committed.as_ref(), || {
        documents.delete(1).unwrap();
    })?;
    verify_view(private.as_ref(), || {})?;
    let mut private_rows = 0;
    private.for_each_fields_multi_ref_with_presence(
        &[2],
        &["private", "private"],
        &mut |id, present, values| {
            assert_eq!(id, 2);
            assert!(present);
            assert_eq!(values, [&Value::Int(2), &Value::Int(2)]);
            assert!(std::ptr::eq(values[0], values[1]));
            private_rows += 1;
            true
        },
    )?;
    assert_eq!(private_rows, 1);
    assert!(documents.get(2)?.is_none());
    documents.put_stored(1, document())?;
    Ok(())
}

/// Verify selected fields and metadata after all original handles have closed.
pub fn verify_document_projections_reopen(
    store: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let documents = KeyValueDocumentStore::new(store.clone(), "projection_docs");
    verify_view(&documents, || {})?;
    assert!(documents.get(2)?.is_none());
    Ok(())
}
