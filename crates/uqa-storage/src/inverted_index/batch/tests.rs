//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Batch atomicity, untouched allocation ownership, and ordered update equivalence.

use super::*;
use proptest::prelude::*;
use uqa_analysis::whitespace_analyzer;

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
    assert_eq!(left.doc_count, right.doc_count);
    assert_eq!(left.total_length, right.total_length);
    assert_eq!(left.field_doc_counts, right.field_doc_counts);
    assert_eq!(left.doc_fields, right.doc_fields);
    assert_eq!(left.doc_terms, right.doc_terms);
    assert_eq!(format!("{:?}", left.index), format!("{:?}", right.index));
}

#[test]
fn batches_preserve_untouched_posting_allocations() {
    let mut index = seeded();
    let key = ("body".into(), crate::TokenTermKey::from_text("shared"));
    let occurrences = index.index[&key][&99].occurrences.as_ptr();
    let positions = index.index[&key][&99].projection.payload.positions.as_ptr();
    index.try_add_documents(Vec::new()).unwrap();
    assert_eq!(index.index[&key][&99].occurrences.as_ptr(), occurrences);
    index
        .try_add_documents(vec![(1, fields("new shared")), (3, fields("new"))])
        .unwrap();
    assert_eq!(index.index[&key][&99].occurrences.as_ptr(), occurrences);
    assert_eq!(
        index.index[&key][&99].projection.payload.positions.as_ptr(),
        positions
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
            index.total_length.insert("body".into(), u64::MAX - 1);
        } else {
            index.doc_count = u64::MAX - 1;
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
            index.doc_fields.remove(&2);
        } else {
            index
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
    }
}
