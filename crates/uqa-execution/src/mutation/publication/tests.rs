//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::cell::RefCell;

use super::*;
use uqa_storage::document_store::Document;

#[derive(Default)]
struct TextRecorder {
    batches: RefCell<Vec<(String, PreparedFtsDocuments)>>,
}

impl MutationTextIndex for TextRecorder {
    fn text_fields(
        &self,
        _table: &str,
        _document: &Document,
    ) -> Result<BTreeMap<String, String>, SQLError> {
        unreachable!("the test supplies already evaluated fields")
    }

    fn add_documents(&self, table: &str, documents: PreparedFtsDocuments) -> Result<(), SQLError> {
        self.batches.borrow_mut().push((table.into(), documents));
        Ok(())
    }
}

#[test]
fn repeated_identity_publishes_intermediate_text_before_replacement() {
    let text = TextRecorder::default();
    let mut batch = MutationPublicationBatch::default();
    let mut add = |table: &str, doc_id, value: &str| {
        batch.before_document(&text, table, doc_id).unwrap();
        batch.push_fts(
            table.into(),
            doc_id,
            BTreeMap::from([("body".into(), value.into())]),
        );
    };
    add("a", 9, "first");
    add("a", 2, "second");
    add("b", 9, "separate relation");
    assert!(text.batches.borrow().is_empty());
    add("a", 9, "replacement");
    assert_eq!(text.batches.borrow().len(), 2);
    batch.flush_fts(&text).unwrap();
    let published = text.batches.borrow();
    assert_eq!(published[0].0, "a");
    assert_eq!(published[0].1[0].1["body"], "first");
    assert_eq!(published[0].1[1].0, 2);
    assert_eq!(published[1].0, "b");
    assert_eq!(published[2].1[0].1["body"], "replacement");
}

#[test]
fn bounded_batches_retain_empty_replacements_and_reset_identity_tracking() {
    let text = TextRecorder::default();
    let mut batch = MutationPublicationBatch::default();
    for id in (0..PREPARED_FTS_BATCH_DOCUMENTS as u64).rev() {
        batch.before_document(&text, "docs", id).unwrap();
        batch.push_fts("docs".into(), id, BTreeMap::new());
    }
    assert!(batch.fts_is_full());
    assert!(text.batches.borrow().is_empty());
    batch.flush_fts(&text).unwrap();
    assert!(!batch.fts_is_full());
    batch.before_document(&text, "docs", 0).unwrap();
    batch.push_fts("docs".into(), 0, BTreeMap::new());
    batch.flush_fts(&text).unwrap();
    batch.flush_fts(&text).unwrap();
    let published = text.batches.borrow();
    assert_eq!(published.len(), 2);
    assert_eq!(published[0].1.len(), PREPARED_FTS_BATCH_DOCUMENTS);
    assert_eq!(published[1].1.len(), 1);
    assert!(published[0].1.iter().all(|(_, fields)| fields.is_empty()));
}
