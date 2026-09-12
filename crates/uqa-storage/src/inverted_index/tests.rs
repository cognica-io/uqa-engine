//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_analysis::analyzer::standard_analyzer;

fn fields<const N: usize>(pairs: [(&str, &str); N]) -> BTreeMap<FieldName, String> {
    pairs
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn add_document_indexes_tokens() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "The Rust Programming Language")]))
        .unwrap();
    idx.add_document(2, fields([("title", "Programming with Rust")]))
        .unwrap();

    let pl = idx.get_posting_list("title", "rust").unwrap();
    let docs: Vec<_> = pl.doc_ids().collect();
    assert_eq!(docs, vec![1, 2]);

    // standard analyzer stems "programming" -> "program"
    let pl2 = idx.get_posting_list("title", "program").unwrap();
    let docs2: Vec<_> = pl2.doc_ids().collect();
    assert_eq!(docs2, vec![1, 2]);
}

#[test]
fn doc_freq_counts_documents() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.add_document(2, fields([("title", "rust rust rust")]))
        .unwrap();
    idx.add_document(3, fields([("title", "go")])).unwrap();

    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 2);
    assert_eq!(idx.doc_freq("title", "go").unwrap(), 1);
    assert_eq!(idx.doc_freq("title", "java").unwrap(), 0);
}

#[test]
fn term_freq_counts_positions() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust rust rust")]))
        .unwrap();
    // After standard analyzer: ["rust", "rust", "rust"]
    assert_eq!(idx.get_term_freq(1, "title", "rust").unwrap(), 3);
}

#[test]
fn doc_length_tracks_token_count() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    // standard analyzer drops "the" / "is" stop words
    idx.add_document(1, fields([("title", "the rust language is fast")]))
        .unwrap();
    // Remaining tokens: ["rust", "languag", "fast"] -> 3
    assert_eq!(idx.get_doc_length(1, "title").unwrap(), 3);
}

#[test]
fn remove_document_clears_postings_and_lengths() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.add_document(2, fields([("title", "rust")])).unwrap();
    idx.remove_document(1).unwrap();

    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(idx.get_doc_length(1, "title").unwrap(), 0);
    assert_eq!(idx.doc_count().unwrap(), 1);
}

#[test]
fn replacing_doc_replaces_postings() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.add_document(1, fields([("title", "go")])).unwrap();

    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 0);
    assert_eq!(idx.doc_freq("title", "go").unwrap(), 1);
    assert_eq!(idx.doc_count().unwrap(), 1);
}

#[test]
fn empty_field_map_removes_existing_index_document() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.add_document(1, BTreeMap::new()).unwrap();
    idx.add_document(2, BTreeMap::new()).unwrap();

    assert_eq!(idx.doc_count().unwrap(), 0);
    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 0);
    assert!(!idx.doc_terms.contains_key(&1));
    assert!(!idx.doc_terms.contains_key(&2));
}

#[test]
fn stats_avg_doc_length_correct() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust language")]))
        .unwrap();
    idx.add_document(2, fields([("title", "rust")])).unwrap();
    let s = idx.stats().unwrap();
    assert_eq!(s.total_docs, 2);
    // 2 + 1 = 3 tokens / 2 docs = 1.5
    assert!((s.avg_doc_length - 1.5).abs() < 1e-9);
    assert_eq!(s.doc_freq("title", "rust"), 2);
}

#[test]
fn token_position_format_accepts_last_u32_position_only() {
    validate_token_position_count(u64::from(u32::MAX) + 1).unwrap();
    let error = validate_token_position_count(u64::from(u32::MAX) + 2).unwrap_err();
    assert!(error.to_string().contains("u32 index format"));
}

#[test]
fn add_overflow_does_not_partially_insert_document() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.doc_count = u64::MAX;

    let error = idx
        .add_document(7, fields([("title", "rust")]))
        .unwrap_err();
    assert!(error.to_string().contains("document count"));
    assert_eq!(idx.doc_count, u64::MAX);
    assert!(!idx.doc_terms.contains_key(&7));
    assert!(idx.get_posting_list("title", "rust").unwrap().is_empty());
}

#[test]
fn field_length_overflow_preserves_existing_document() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.total_length.insert("title".into(), u64::MAX);

    let error = idx.add_document(2, fields([("title", "go")])).unwrap_err();
    assert!(error.to_string().contains("total field length"));
    assert_eq!(idx.doc_count().unwrap(), 1);
    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(idx.doc_freq("title", "go").unwrap(), 0);
    assert!(!idx.doc_terms.contains_key(&2));
}

#[test]
fn corrupt_counter_rejects_remove_without_mutating_postings() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields([("title", "rust")])).unwrap();
    idx.total_length.insert("title".into(), 0);

    let error = idx.remove_document(1).unwrap_err();
    assert!(error.to_string().contains("total field length"));
    assert_eq!(idx.doc_count().unwrap(), 1);
    assert_eq!(idx.doc_freq("title", "rust").unwrap(), 1);
    assert_eq!(idx.get_doc_length(1, "title").unwrap(), 1);
}

#[test]
fn stats_reports_cross_field_total_overflow() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.doc_count = 1;
    idx.total_length.insert("a".into(), u64::MAX);
    idx.total_length.insert("b".into(), 1);

    let error = idx.stats().unwrap_err();
    assert!(error.to_string().contains("total document length"));
}
