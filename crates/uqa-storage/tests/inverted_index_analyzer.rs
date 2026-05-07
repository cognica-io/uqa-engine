//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of the analyzer-integration suite from
//! `uqa/tests/test_analysis.py`: per-field analyzer overrides on
//! `MemoryInvertedIndex` / `SQLiteInvertedIndex`, dual-phase
//! (index/search/both) analyzer assignment, and the search-time
//! synonym fallback chain.

use std::collections::BTreeMap;

use uqa_analysis::{
    keyword_analyzer, standard_analyzer, whitespace_analyzer, Analyzer, TokenFilter, Tokenizer,
};
use uqa_storage::sqlite::{Catalog, ManagedConnection};
use uqa_storage::{AnalyzerPhase, InvertedIndex, MemoryInvertedIndex, SQLiteInvertedIndex};

fn sqlite_with_catalog() -> ManagedConnection {
    let conn = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(conn.clone()).unwrap();
    conn
}

fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

// =====================================================================
// MemoryInvertedIndex per-field analyzer
// =====================================================================

#[test]
fn memory_default_analyzer_drops_stop_words() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields(&[("title", "The Quick Brown Fox")]));
    assert_eq!(idx.get_posting_list("title", "the").len(), 0);
    assert_eq!(idx.get_posting_list("title", "quick").len(), 1);
}

#[test]
fn memory_custom_analyzer_via_constructor() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.add_document(1, fields(&[("title", "The Quick Brown Fox")]));
    assert_eq!(idx.get_posting_list("title", "the").len(), 0);
    assert_eq!(idx.get_posting_list("title", "quick").len(), 1);
}

#[test]
fn memory_per_field_analyzer() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.set_field_analyzer("title", standard_analyzer("english"), AnalyzerPhase::Index)
        .unwrap();
    idx.set_field_analyzer("body", whitespace_analyzer(), AnalyzerPhase::Index)
        .unwrap();
    idx.add_document(
        1,
        fields(&[("title", "The Quick Fox"), ("body", "The body")]),
    );
    // standard drops "the" from title; whitespace keeps "the" in body
    assert_eq!(idx.get_posting_list("title", "the").len(), 0);
    assert_eq!(idx.get_posting_list("body", "the").len(), 1);
}

#[test]
fn memory_get_field_analyzer_falls_back_to_default() {
    let idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    let a = idx.get_field_analyzer("missing");
    // default analyzer is the constructor's; analyzing a stop word
    // returns an empty token list.
    assert!(a.analyze("the").is_empty());
}

#[test]
fn memory_per_field_search_falls_back_to_index() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    let custom = standard_analyzer("english");
    idx.set_field_analyzer("body", custom, AnalyzerPhase::Index)
        .unwrap();
    // No search analyzer set: search should fall back to index
    let search = idx.get_search_analyzer("body");
    assert!(search.analyze("the").is_empty());
}

#[test]
fn memory_search_falls_back_to_default_when_no_field_analyzer() {
    let idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    let search = idx.get_search_analyzer("body");
    assert!(search.analyze("the").is_empty());
}

#[test]
fn memory_phase_both_sets_both() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.set_field_analyzer("body", standard_analyzer("english"), AnalyzerPhase::Both)
        .unwrap();
    let index_a = idx.get_field_analyzer("body");
    let search_a = idx.get_search_analyzer("body");
    assert!(index_a.analyze("the").is_empty());
    assert!(search_a.analyze("the").is_empty());
}

#[test]
fn memory_separate_phases() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    let idx_a = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Lowercase],
        Vec::new(),
    );
    let mut syn = BTreeMap::new();
    syn.insert("car".to_string(), vec!["automobile".to_string()]);
    let search_a = Analyzer::new(
        Tokenizer::Whitespace,
        vec![
            TokenFilter::Lowercase,
            TokenFilter::Synonym {
                synonyms: syn,
                synonyms_path: None,
            },
        ],
        Vec::new(),
    );
    idx.set_field_analyzer("body", idx_a, AnalyzerPhase::Index)
        .unwrap();
    idx.set_field_analyzer("body", search_a, AnalyzerPhase::Search)
        .unwrap();
    let resolved_search = idx.get_search_analyzer("body");
    assert!(resolved_search
        .analyze("car")
        .contains(&"automobile".to_string()));
    let resolved_index = idx.get_field_analyzer("body");
    assert!(!resolved_index
        .analyze("car")
        .contains(&"automobile".to_string()));
}

#[test]
fn memory_invalid_phase_string_rejected() {
    let r = AnalyzerPhase::from_str("bad");
    assert!(r.is_err());
}

#[test]
fn memory_phase_string_parses() {
    assert_eq!(
        AnalyzerPhase::from_str("index").unwrap(),
        AnalyzerPhase::Index
    );
    assert_eq!(
        AnalyzerPhase::from_str("search").unwrap(),
        AnalyzerPhase::Search
    );
    assert_eq!(
        AnalyzerPhase::from_str("query").unwrap(),
        AnalyzerPhase::Search
    );
    assert_eq!(
        AnalyzerPhase::from_str("both").unwrap(),
        AnalyzerPhase::Both
    );
}

#[test]
fn memory_keyword_analyzer_for_title_field() {
    let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
    idx.set_field_analyzer("title", keyword_analyzer(), AnalyzerPhase::Both)
        .unwrap();
    idx.add_document(1, fields(&[("title", "Hello World")]));
    // Keyword analyzer keeps the entire string as one token.
    assert_eq!(idx.get_posting_list("title", "Hello World").len(), 1);
    assert_eq!(idx.get_posting_list("title", "hello").len(), 0);
}

// =====================================================================
// SQLiteInvertedIndex per-field analyzer
// =====================================================================

#[test]
fn sqlite_default_analyzer() {
    let conn = sqlite_with_catalog();
    let mut idx = SQLiteInvertedIndex::new(conn, "test_table", standard_analyzer("english"));
    idx.add_document(1, fields(&[("title", "Hello World")]));
    assert_eq!(idx.get_posting_list("title", "hello").len(), 1);
}

#[test]
fn sqlite_custom_analyzer_via_constructor() {
    let conn = sqlite_with_catalog();
    let mut idx = SQLiteInvertedIndex::new(conn, "test_table", standard_analyzer("english"));
    idx.add_document(1, fields(&[("title", "The Quick Brown Fox")]));
    assert_eq!(idx.get_posting_list("title", "the").len(), 0);
    assert_eq!(idx.get_posting_list("title", "quick").len(), 1);
}

#[test]
fn sqlite_per_field_analyzer() {
    let conn = sqlite_with_catalog();
    let mut idx = SQLiteInvertedIndex::new(conn, "test_table", standard_analyzer("english"));
    idx.set_field_analyzer("title", standard_analyzer("english"), AnalyzerPhase::Index)
        .unwrap();
    idx.set_field_analyzer("body", whitespace_analyzer(), AnalyzerPhase::Index)
        .unwrap();
    idx.add_document(
        1,
        fields(&[("title", "The Quick Fox"), ("body", "The body text")]),
    );
    assert_eq!(idx.get_posting_list("title", "the").len(), 0);
    assert_eq!(idx.get_posting_list("body", "the").len(), 1);
}

#[test]
fn sqlite_tokenize_uses_analyzer() {
    let conn = sqlite_with_catalog();
    let mut idx = SQLiteInvertedIndex::new(conn, "test_table", standard_analyzer("english"));
    let tokens = idx.tokenize("Hello World", "title");
    assert_eq!(tokens, vec!["hello", "world"]);

    idx.set_field_analyzer("title", keyword_analyzer(), AnalyzerPhase::Both)
        .unwrap();
    let tokens2 = idx.tokenize("Hello World", "title");
    assert_eq!(tokens2, vec!["Hello World"]);
}

#[test]
fn sqlite_dual_analyzer_separate_phases() {
    let conn = sqlite_with_catalog();
    let mut idx = SQLiteInvertedIndex::new(conn, "test", standard_analyzer("english"));
    let idx_a = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Lowercase],
        Vec::new(),
    );
    let mut syn = BTreeMap::new();
    syn.insert("car".to_string(), vec!["auto".to_string()]);
    let search_a = Analyzer::new(
        Tokenizer::Whitespace,
        vec![
            TokenFilter::Lowercase,
            TokenFilter::Synonym {
                synonyms: syn,
                synonyms_path: None,
            },
        ],
        Vec::new(),
    );
    idx.set_field_analyzer("body", idx_a, AnalyzerPhase::Index)
        .unwrap();
    idx.set_field_analyzer("body", search_a, AnalyzerPhase::Search)
        .unwrap();
    let resolved_search = idx.get_search_analyzer("body");
    assert!(resolved_search.analyze("car").contains(&"auto".to_string()));
}

#[test]
fn sqlite_backward_compat_no_phase_arg_uses_both() {
    // The Rust API requires an explicit phase, so the "no phase"
    // backwards-compat check is exercised via `Phase::Both`.
    let conn = sqlite_with_catalog();
    let mut idx = SQLiteInvertedIndex::new(conn, "test", standard_analyzer("english"));
    idx.set_field_analyzer("body", standard_analyzer("english"), AnalyzerPhase::Both)
        .unwrap();
    let i = idx.get_field_analyzer("body");
    let s = idx.get_search_analyzer("body");
    assert!(i.analyze("the").is_empty());
    assert!(s.analyze("the").is_empty());
}
