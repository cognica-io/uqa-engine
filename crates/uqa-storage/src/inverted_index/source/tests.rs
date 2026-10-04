//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;

use uqa_core::{CancellationToken, Value};

use super::*;
use crate::document_store::Document;
use crate::{DocumentStore, MemoryDocumentStore};

fn read_all(source: &mut dyn TextIndexSource) -> Vec<(DocId, BTreeMap<FieldName, String>)> {
    let mut documents = Vec::new();
    while let Some(document) = source.next_document().unwrap() {
        documents.push(document);
    }
    documents
}

fn body(text: &str) -> BTreeMap<FieldName, String> {
    BTreeMap::from([("body".to_owned(), text.to_owned())])
}

#[test]
fn listed_documents_are_read_in_identity_order_keeping_the_last_copy() {
    let mut source = TextIndexDocuments::new(vec![
        (5, body("first five")),
        (1, body("one")),
        (5, body("second five")),
        (3, body("three")),
    ]);
    assert_eq!(
        read_all(&mut source),
        [
            (1, body("one")),
            (3, body("three")),
            (5, body("second five"))
        ]
    );
}

#[test]
fn a_document_store_is_read_a_page_at_a_time_without_documents_lacking_text() {
    let mut store = MemoryDocumentStore::new();
    let large = "word ".repeat(2_000);
    for doc_id in 1..=2_500_u64 {
        let mut document = Document::new();
        // Every third document holds no text in the indexed field, and long texts end pages by their bytes.
        let value = match doc_id % 3 {
            0 => Value::Int(i64::try_from(doc_id).unwrap()),
            1 => Value::Str(large.clone()),
            _ => Value::Str(format!("short {doc_id}")),
        };
        document.insert("body".to_owned(), value);
        document.insert("other".to_owned(), Value::Str("unindexed".to_owned()));
        store.put(doc_id, document).unwrap();
    }
    let mut source = DocumentTextSource::new(Arc::new(store), vec!["body".to_owned()], None);
    let documents = read_all(&mut source);
    let expected = (1..=2_500_u64)
        .filter(|doc_id| doc_id % 3 != 0)
        .collect::<Vec<_>>();
    assert_eq!(
        documents
            .iter()
            .map(|(doc_id, _)| *doc_id)
            .collect::<Vec<_>>(),
        expected
    );
    assert!(documents.iter().all(|(doc_id, fields)| {
        fields.len() == 1
            && fields["body"]
                == if doc_id % 3 == 1 {
                    large.clone()
                } else {
                    format!("short {doc_id}")
                }
    }));
}

#[test]
fn a_cancelled_document_store_source_stops() {
    let mut store = MemoryDocumentStore::new();
    store
        .put(
            1,
            Document::from([("body".to_owned(), Value::Str("text".to_owned()))]),
        )
        .unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut source =
        DocumentTextSource::new(Arc::new(store), vec!["body".to_owned()], Some(cancellation));
    assert!(source.next_document().is_err());
}
