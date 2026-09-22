//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::clustered_postings::PostingReadCursor;
use crate::document_store::{DocumentMetadata, StoredDocument};
use crate::read_control::StorageReadControl;
use crate::{
    AnalyzerPhase, DocumentStore, InvertedIndex, MemoryDocumentStore, MemoryInvertedIndex,
    MemoryVectorIndex, TokenTermKey, VectorIndex,
};
use uqa_core::Value;

#[test]
fn document_adapter_preserves_shared_projections_and_rejects_writes() {
    let mut live = MemoryDocumentStore::new();
    live.put_stored(
        7,
        StoredDocument::with_metadata(
            [("body".into(), Value::Str("retained".into()))].into(),
            DocumentMetadata::with_tuple_xmin(19),
        ),
    )
    .unwrap();
    let snapshot = live.snapshot().unwrap();
    let mut retained = ReadOnlySnapshot::new(Arc::clone(&snapshot));
    let direct = snapshot
        .get_shared_fields(&[7], &["body"])
        .unwrap()
        .unwrap();
    let adapted = retained
        .get_shared_fields(&[7], &["body"])
        .unwrap()
        .unwrap();
    assert!(std::ptr::eq(
        direct[0].as_ref().unwrap().indexed_values().0,
        adapted[0].as_ref().unwrap().indexed_values().0
    ));
    live.clear().unwrap();
    drop(live);
    drop(snapshot);
    assert_eq!(
        retained.get_metadata(7).unwrap().unwrap().tuple_xmin(),
        Some(19)
    );
    let mut rows = Vec::new();
    assert_eq!(
        retained
            .for_each_next_fields(None, 2, &["body"], &mut |id, values| {
                rows.push((id, values[0].clone()));
                true
            })
            .unwrap(),
        Some(1)
    );
    assert_eq!(rows, vec![(7, Value::Str("retained".into()))]);
    assert!(retained.clear().is_err());
    assert!(retained
        .patch_fields(7, &std::collections::BTreeMap::new())
        .is_err());
    assert!(retained.writable_snapshot().is_err());
    let mut second = retained.snapshot().unwrap();
    drop(retained);
    assert!(Arc::get_mut(&mut second).unwrap().delete(7).is_err());
    assert_eq!(second.len().unwrap(), 1);
}

#[test]
fn text_adapter_preserves_revisions_borrowed_reads_budgets_and_cancellation() {
    let mut live = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    live.add_document(7, [("body".into(), "old old".into())].into())
        .unwrap();
    let revision = live.index_analyzer_revision("body").unwrap();
    let key = TokenTermKey::from_text("old");
    let occurrences = live.get_occurrences(7, "body", &key).unwrap();
    let mut retained = ReadOnlySnapshot::new(live.snapshot().unwrap());
    live.clear().unwrap();
    drop(live);
    assert!(Arc::ptr_eq(
        &retained.index_analyzer_revision("body").unwrap(),
        &revision
    ));
    assert_eq!(
        retained.get_occurrences(7, "body", &key).unwrap(),
        occurrences
    );
    let control = StorageReadControl::with_limit(4096);
    {
        let cursor = retained
            .posting_read_cursor_key_budgeted("body", &key, &control)
            .unwrap();
        assert_eq!(cursor.current().unwrap().doc_id, 7);
        assert_eq!(cursor.current().unwrap().term_freq, 2);
        assert!(control.memory().used() > 0);
    }
    assert_eq!(control.memory().used(), 0);
    assert!(retained
        .posting_read_cursor_key_budgeted("body", &key, &StorageReadControl::with_limit(0))
        .is_err());
    control.cancellation().cancel();
    assert!(retained
        .get_occurrences_budgeted(7, "body", &key, &control)
        .is_err());
    assert!(retained.clear().is_err());
    assert!(retained
        .set_field_analyzer_revision("body", revision, AnalyzerPhase::Both)
        .is_err());
    assert!(retained.writable_snapshot().is_err());
    assert_eq!(retained.doc_freq("body", "old").unwrap(), 1);
}

#[test]
fn vector_adapter_keeps_canonical_membership_and_index_kind() {
    let indexes: [Box<dyn VectorIndex>; 3] = [
        Box::new(MemoryVectorIndex::new(2)),
        Box::new(crate::IVFIndex::new(2)),
        Box::new(crate::HNSWIndex::new(2)),
    ];
    for mut live in indexes {
        live.add(7, vec![1.0, 0.0]).unwrap();
        let kind = live.index_kind();
        let mut retained = ReadOnlySnapshot::new(live.snapshot().unwrap());
        live.clear().unwrap();
        drop(live);
        assert_eq!(retained.index_kind(), kind);
        assert!(retained.contains_document(7).unwrap());
        assert_eq!(retained.count().unwrap(), 1);
        assert_eq!(
            retained
                .search_threshold(&[1.0, 0.0], 0.9)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            vec![7]
        );
        assert!(retained.initialize().is_err());
        assert!(retained.clear().is_err());
        assert!(retained.writable_snapshot().is_err());
    }
}
