//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::clustered_postings::PostingReadCursor;
use crate::inverted_index::{retained::tests::corpus_bytes, AnalyzerPhase};
use crate::{StorageBackendError, TokenTermKey};
use std::collections::BTreeMap;
use uqa_core::memory::{MemoryBudget, MemoryError, MemoryReservation};
use uqa_core::CancellationToken;

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}

fn seeded() -> MemoryInvertedIndex {
    let mut index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    index
        .add_document(1, fields("old shared shared β"))
        .unwrap();
    index.add_document(2, fields("other shared")).unwrap();
    index
}

fn capture_bytes(index: &MemoryInvertedIndex) -> usize {
    corpus_bytes(&index.state)
        + size_of::<MemoryInvertedIndex>()
        + 2 * size_of::<MemoryReservation>()
}

pub(in crate::inverted_index) fn assert_accounted(index: &MemoryInvertedIndex) {
    let control = StorageReadControl::with_limit(16 << 20);
    let capture = index.snapshot_with_control(&control).unwrap();
    assert_eq!(control.memory().used(), capture_bytes(index));
    drop(capture);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn shared_capture_keeps_occurrences_revisions_and_metadata_after_source_replacement() {
    let mut index = seeded();
    let revision = index.index_analyzer_revision("body").unwrap();
    let search = index.search_analyzer_revision("body").unwrap();
    let term = TokenTermKey::from_text("shared");
    let occurrences = index.get_occurrences(1, "body", &term).unwrap();
    let metadata = index.indexed_field_metadata(1, "body").unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    let capture = index.snapshot_with_control(&control).unwrap();
    assert_eq!(Arc::strong_count(&index.state), 2);
    let bytes = control.memory().used();
    let nested = capture
        .snapshot()
        .unwrap()
        .snapshot_with_control(&StorageReadControl::with_limit(0))
        .unwrap();
    assert_eq!(control.memory().used(), bytes);
    assert_eq!(Arc::strong_count(&index.state), 2);
    let old_state = Arc::as_ptr(&index.state);
    index.add_document(1, fields("replacement")).unwrap();
    assert_ne!(Arc::as_ptr(&index.state), old_state);
    index
        .set_field_analyzer(
            "body",
            uqa_analysis::keyword_analyzer(),
            AnalyzerPhase::Search,
        )
        .unwrap();
    index.clear().unwrap();
    drop(index);
    drop(capture);
    assert_eq!(
        nested.get_occurrences(1, "body", &term).unwrap(),
        occurrences
    );
    assert_eq!(nested.indexed_field_metadata(1, "body").unwrap(), metadata);
    assert!(Arc::ptr_eq(
        &nested.index_analyzer_revision("body").unwrap(),
        &revision
    ));
    assert!(Arc::ptr_eq(
        &nested.search_analyzer_revision("body").unwrap(),
        &search
    ));
    assert_eq!(nested.doc_count().unwrap(), 2);
    assert_eq!(control.memory().used(), bytes);
    drop(nested);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn retention_matches_live_capacities_through_each_mutation_and_copy_boundary() {
    let mut index = seeded();
    assert_accounted(&index);
    index
        .add_document(1, fields(&"shared longterm ".repeat(17)))
        .unwrap();
    assert_accounted(&index);
    index
        .add_document(2, BTreeMap::from([("empty".into(), String::new())]))
        .unwrap();
    assert_accounted(&index);
    index.remove_document(1).unwrap();
    index.remove_document(77).unwrap();
    assert_accounted(&index);
    index
        .try_add_documents(vec![
            (2, fields("first shared")),
            (7, fields("shared separate")),
            (2, BTreeMap::new()),
            (7, fields("final shared shared")),
            (9, BTreeMap::from([("title".into(), "shared".into())])),
        ])
        .unwrap();
    assert_accounted(&index);
    let held = index
        .snapshot_with_control(&StorageReadControl::with_limit(1 << 20))
        .unwrap();
    let cloned = index.clone();
    assert_accounted(&cloned);
    let prior = Arc::as_ptr(&index.state);
    index
        .try_add_documents(vec![(7, fields("copied")), (9, BTreeMap::new())])
        .unwrap();
    assert_ne!(Arc::as_ptr(&index.state), prior);
    assert_accounted(&index);
    assert_eq!(held.get_term_freq(7, "body", "shared").unwrap(), 2);
    index
        .try_rebuild_documents(vec![
            (1, fields("rebuilt")),
            (1, fields("last")),
            (2, fields("last")),
        ])
        .unwrap();
    assert_accounted(&index);
    index.clear().unwrap();
    assert_accounted(&index);
    index.add_document(3, fields("reused")).unwrap();
    assert_accounted(&index);
}

#[test]
fn quota_and_cancellation_reject_capture_without_leaking_or_changing_the_source() {
    let index = seeded();
    let required = capture_bytes(&index);
    for limit in [0, size_of::<MemoryInvertedIndex>(), required - 1, required] {
        let control = StorageReadControl::with_limit(limit);
        let result = index.snapshot_with_control(&control);
        if limit < required {
            assert!(matches!(
                result,
                Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
            ));
        } else {
            let capture = result.unwrap();
            assert_eq!(control.memory().used(), required);
            assert_eq!(capture.doc_count().unwrap(), 2);
            drop(capture);
        }
        assert_eq!(control.memory().used(), 0);
        assert_eq!(Arc::strong_count(&index.state), 1);
        assert_eq!(index.doc_freq("body", "shared").unwrap(), 2);
    }
    let cancelled = StorageReadControl::with_limit(0);
    cancelled.cancellation().cancel();
    assert!(matches!(
        index.snapshot_with_control(&cancelled),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(cancelled.memory().used(), 0);
    assert_accounted(&index);
}

#[test]
fn independent_readers_share_retention_without_replacing_explicit_read_limits_or_tokens() {
    let index = seeded();
    let memory = MemoryBudget::new(1 << 20);
    let first = StorageReadControl::new(&memory, &CancellationToken::new());
    let second = StorageReadControl::new(&memory, &CancellationToken::new());
    let left = index.snapshot_with_control(&first).unwrap();
    let initial = memory.used();
    // Fail the final wrapper reservation after admitting the generation in a different allowance.
    let rejected = StorageReadControl::with_limit(capture_bytes(&index) - 1);
    assert!(matches!(
        index.snapshot_with_control(&rejected),
        Err(StorageBackendError::Memory(_))
    ));
    assert_eq!(rejected.memory().used(), 0);
    let cancelled = StorageReadControl::with_limit(capture_bytes(&index));
    let cancelled_capture = index.state.retention.with_retained(&cancelled, |memory| {
        cancelled.cancellation().cancel();
        Ok(memory)
    });
    assert!(matches!(
        cancelled_capture,
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(cancelled.memory().used(), 0);
    let right = index.snapshot_with_control(&second).unwrap();
    assert_eq!(
        memory.used() - initial,
        size_of::<MemoryInvertedIndex>() + size_of::<MemoryReservation>()
    );
    assert!(matches!(
        index.snapshot_with_control(&StorageReadControl::with_limit(0)),
        Err(StorageBackendError::Memory(_))
    ));
    let term = TokenTermKey::from_text("shared");
    first.cancellation().cancel();
    assert!(matches!(
        left.doc_count(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert!(matches!(
        left.snapshot_with_control(&second),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(right.doc_count().unwrap(), 2);
    let workspace = StorageReadControl::with_limit(1 << 16);
    let full = memory.reserve(memory.limit() - memory.used()).unwrap();
    let occurrences = right
        .get_occurrences_budgeted(1, "body", &term, &workspace)
        .unwrap();
    assert_eq!(occurrences.len(), 2);
    assert!(workspace.memory().used() > 0);
    assert_eq!(memory.used(), memory.limit());
    drop(occurrences);
    assert_eq!(workspace.memory().used(), 0);
    drop(full);
    assert!(matches!(
        right.get_occurrences_budgeted(1, "body", &term, &StorageReadControl::with_limit(0)),
        Err(StorageBackendError::Memory(_))
    ));
    let mut cursor = right
        .posting_read_cursor_key_budgeted("body", &term, &workspace)
        .unwrap();
    assert_eq!(cursor.current().unwrap().doc_id, 1);
    second.cancellation().cancel();
    assert!(matches!(
        cursor.advance(),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(cursor.current().unwrap().doc_id, 1);
    second.cancellation().reset();
    workspace.cancellation().cancel();
    assert!(matches!(
        cursor.advance(),
        Err(StorageBackendError::Cancelled(_))
    ));
    workspace.cancellation().reset();
    assert_eq!(cursor.advance().unwrap().unwrap().doc_id, 2);
    drop(cursor);
    drop(left);
    drop(right);
    assert_eq!(memory.used(), 0);
    assert_eq!(workspace.memory().used(), 0);
}
