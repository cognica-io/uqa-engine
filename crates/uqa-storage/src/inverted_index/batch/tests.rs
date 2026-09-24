//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Batch atomicity, untouched allocation ownership, and ordered update equivalence.

use super::*;
use crate::InvertedIndex;
use proptest::prelude::*;
use uqa_analysis::whitespace_analyzer;

#[test]
fn evaluated_text_changes_preserve_logical_terms_and_atomicity() {
    crate::key_value::conformance::verify_inverted_index_changes(&mut MemoryInvertedIndex::new(
        whitespace_analyzer(),
    ))
    .unwrap();
}

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}

fn seeded() -> MemoryInvertedIndex {
    let mut index = MemoryInvertedIndex::new(whitespace_analyzer());
    index.add_document(1, fields("old shared")).unwrap();
    index.add_document(2, fields("other shared")).unwrap();
    index.add_document(99, fields("unrelated shared")).unwrap();
    index
}

fn assert_state(left: &MemoryInvertedIndex, right: &MemoryInvertedIndex) {
    assert_eq!(left.state.doc_count, right.state.doc_count);
    assert_eq!(left.state.field_counters, right.state.field_counters);
    assert_eq!(left.state.documents, right.state.documents);
    assert_eq!(
        format!("{:?}", left.state.index),
        format!("{:?}", right.state.index)
    );
}

#[test]
fn batches_preserve_untouched_posting_allocations() {
    let mut index = seeded();
    let key = ("body".into(), crate::TokenTermKey::from_text("shared"));
    let occurrences = index.state.index[&key][&99].occurrences.as_ptr();
    index.try_add_documents(Vec::new()).unwrap();
    assert_eq!(
        index.state.index[&key][&99].occurrences.as_ptr(),
        occurrences
    );
    index
        .try_add_documents(vec![(1, fields("new shared")), (3, fields("new"))])
        .unwrap();
    assert_eq!(
        index.state.index[&key][&99].occurrences.as_ptr(),
        occurrences
    );
    assert_eq!(index.doc_count().unwrap(), 4);
    assert_eq!(index.doc_freq("body", "old").unwrap(), 0);
    assert_eq!(index.doc_freq("body", "new").unwrap(), 2);
}

#[test]
fn late_document_and_field_counter_failures_publish_nothing() {
    for field_overflow in [false, true] {
        let mut index = seeded();
        if field_overflow {
            Arc::make_mut(&mut index.state)
                .field_counters
                .get_mut("body")
                .unwrap()
                .total = u64::MAX - 1;
        } else {
            Arc::make_mut(&mut index.state).doc_count = u64::MAX - 1;
        }
        let before = index.clone();
        let error = index
            .try_add_documents(vec![(3, fields("next")), (4, fields("overflow"))])
            .unwrap_err();
        assert!(error.to_string().contains(if field_overflow {
            "total field length"
        } else {
            "document count"
        }));
        assert_state(&index, &before);
    }
}

#[test]
fn corrupt_affected_postings_or_reverse_metadata_publish_nothing() {
    for missing_field in [false, true] {
        let mut index = seeded();
        if missing_field {
            Arc::make_mut(&mut index.state)
                .documents
                .get_mut(&2)
                .unwrap()
                .fields = OwnedMap::new();
        } else {
            Arc::make_mut(&mut index.state)
                .index
                .remove(&("body".into(), crate::TokenTermKey::from_text("other")));
        }
        let before = index.clone();
        assert!(index
            .try_add_documents(vec![(1, fields("new")), (2, fields("new"))])
            .is_err());
        assert_state(&index, &before);
    }
}

proptest! {
    #[test]
    fn repeated_ids_deletions_and_field_moves_match_ordered_point_updates(
        operations in prop::collection::vec((0_u8..12, 0_u8..4, 0_u8..8), 0..40)
    ) {
        let mut index = seeded();
        let mut expected = index.clone();
        let documents: Vec<_> = operations.into_iter().map(|(id, mask, length)| {
            let mut values = BTreeMap::new();
            if mask & 1 != 0 { values.insert("body".into(), "a b ".repeat(usize::from(length))); }
            if mask & 2 != 0 { values.insert("title".into(), "c ".repeat(usize::from(length))); }
            (u64::from(id), values)
        }).collect();
        for (id, fields) in &documents {
            expected.add_document(*id, fields.clone()).unwrap();
        }
        index.try_add_documents(documents).unwrap();
        assert_state(&index, &expected);
        crate::inverted_index::snapshot::tests::assert_accounted(&index);
        crate::inverted_index::snapshot::tests::assert_accounted(&expected);
    }
}
