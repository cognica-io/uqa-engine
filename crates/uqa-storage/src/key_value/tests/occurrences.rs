//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persisted graph validation and transaction ownership.

use super::{store, Arc, InvertedIndex, KeyValueInvertedIndex};
use crate::key_value::{codec::*, occurrence_keys as keys};
use crate::TokenTermKey;
use uqa_analysis::whitespace_analyzer;

fn fields(text: &str) -> std::collections::BTreeMap<String, String> {
    std::collections::BTreeMap::from([("body".into(), text.into())])
}

#[test]
fn graph_mutations_reject_inconsistent_metadata_without_partial_publication() {
    let store = store();
    let mut index = KeyValueInvertedIndex::new(store.clone(), "docs", whitespace_analyzer());
    index.add_document(1, fields("old value")).unwrap();
    let reverse_key = keys::document_key("docs", keys::DOCUMENT, 1, "body").unwrap();
    let reverse = store.get(&reverse_key).unwrap().unwrap();
    store.delete(&reverse_key).unwrap();
    let corrupted = store.scan_prefix(b"").unwrap();
    for result in [
        index.add_document(1, fields("replacement")),
        index.remove_document(1),
    ] {
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("reverse terms are missing"));
        assert_eq!(store.scan_prefix(b"").unwrap(), corrupted);
    }
    store.put(&reverse_key, &reverse).unwrap();
    let metadata_key = keys::metadata_key("docs", "body", 1).unwrap();
    let metadata = store.get(&metadata_key).unwrap().unwrap();
    let mut wrong_revision = metadata.clone();
    wrong_revision[8] ^= 1;
    store.put(&metadata_key, &wrong_revision).unwrap();
    assert!(index
        .indexed_field_metadata(1, "body")
        .unwrap_err()
        .to_string()
        .contains("revisions disagree"));
    let corrupted = store.scan_prefix(b"").unwrap();
    assert!(index.add_document(1, fields("replacement")).is_err());
    assert_eq!(store.scan_prefix(b"").unwrap(), corrupted);
    store.put(&metadata_key, &metadata).unwrap();
    let mut wrong_length = metadata.clone();
    wrong_length[40..48].copy_from_slice(&3_u64.to_le_bytes());
    store.put(&metadata_key, &wrong_length).unwrap();
    assert!(index
        .get_posting_list("body", "old")
        .unwrap_err()
        .to_string()
        .contains("score length disagrees"));
    assert!(index.get_doc_length(1, "body").is_err());
    store.put(&metadata_key, &metadata).unwrap();
    index.remove_document(1).unwrap();
    assert_eq!(index.doc_count().unwrap(), 0);
}

#[test]
fn occurrence_namespace_rejects_legacy_payloads_and_unknown_or_missing_format_markers() {
    let store = store();
    let mut index = KeyValueInvertedIndex::new(store.clone(), "docs", whitespace_analyzer());
    index.add_document(1, fields("value")).unwrap();
    let marker = keys::kind_prefix("docs", keys::FORMAT).unwrap();
    for value in [None, Some(b"future-format".as_slice())] {
        match value {
            Some(value) => store.put(&marker, value).unwrap(),
            None => store.delete(&marker).unwrap(),
        }
        let before = store.scan_prefix(b"").unwrap();
        assert!(index.source_rebuild_required().is_err());
        assert!(index.get_posting_list("body", "value").is_err());
        assert!(index.add_document(2, fields("new")).is_err());
        assert_eq!(store.scan_prefix(b"").unwrap(), before);
    }
    store.put(&marker, keys::FORMAT_NAME).unwrap();
    let term = TokenTermKey::from_text("value");
    let score_key = keys::cluster_key("docs", keys::SCORE, "body", &term, 0).unwrap();
    let mut score_blob = store.get(&score_key).unwrap().unwrap();
    score_blob[4] = 1;
    store.put(&score_key, &score_blob).unwrap();
    assert!(index.posting_cursor_key("body", &term).is_err());
    assert!(index.get_term_freq_key(1, "body", &term).is_err());
    assert!(index.get_occurrence_postings("body", &term).is_err());
    let before = store.scan_prefix(b"").unwrap();
    assert!(index.remove_document(1).is_err());
    assert_eq!(store.scan_prefix(b"").unwrap(), before);
    index
        .try_rebuild_documents(vec![(1, fields("value"))])
        .unwrap();
    assert_eq!(index.get_term_freq(1, "body", "value").unwrap(), 1);
    assert!(!index.source_rebuild_required().unwrap());
}

#[test]
fn legacy_conversion_joins_the_owning_transaction_and_source_rebuild_retires_all_old_keys() {
    let store = store();
    store
        .put(
            &posting_key("docs", "body", "old", 1).unwrap(),
            &positions_to_blob(&[0]).unwrap(),
        )
        .unwrap();
    store
        .put(&reverse_posting_key("docs", 1, "body", "old").unwrap(), &[])
        .unwrap();
    store
        .put(&doc_length_key("docs", 1, "body").unwrap(), &u64_value(1))
        .unwrap();
    store
        .put(&field_stats_key("docs", "body").unwrap(), &u64_value(1))
        .unwrap();
    let before = store.scan_prefix(b"").unwrap();
    store.begin_transaction().unwrap();
    KeyValueInvertedIndex::migrate_legacy_storage(store.as_ref()).unwrap();
    assert!(store.in_transaction());
    let mut index = KeyValueInvertedIndex::new(Arc::clone(&store), "docs", whitespace_analyzer());
    assert!(index.source_rebuild_required().unwrap());
    index
        .try_rebuild_documents(vec![(1, fields("actual source"))])
        .unwrap();
    assert_eq!(index.get_term_freq(1, "body", "actual").unwrap(), 1);
    assert_eq!(index.get_term_freq(1, "body", "old").unwrap(), 0);
    assert_eq!(
        index
            .indexed_field_metadata(1, "body")
            .unwrap()
            .unwrap()
            .final_offsets
            .end_utf8,
        13
    );
    store.rollback_transaction().unwrap();
    assert_eq!(store.scan_prefix(b"").unwrap(), before);
    assert!(index.source_rebuild_required().unwrap());
    index
        .try_rebuild_documents(vec![(1, fields("actual source"))])
        .unwrap();
    for tag in [b'p', b'r', b'k', b'o', b'x', b'l', b'f'] {
        assert!(store.scan_prefix(&[tag]).unwrap().is_empty());
    }
    assert!(!index.source_rebuild_required().unwrap());
}
