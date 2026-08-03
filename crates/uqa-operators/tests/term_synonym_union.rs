//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Search-time synonym-union coverage for `TermOperator`.
//! Verifies that `TermOperator` resolves the search-time analyzer for
//! the field, expands the user-supplied term through any synonym
//! filter, and unions the resulting per-token posting lists.

use std::collections::BTreeMap;

use uqa_analysis::{Analyzer, TokenFilter, Tokenizer};
use uqa_operators::base::ExecutionContext;
use uqa_operators::primitive::TermOperator;
use uqa_operators::Operator;
use uqa_storage::{AnalyzerPhase, InvertedIndex, MemoryInvertedIndex, StorageBackendError};

fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn synonym_expansion_finds_documents() {
    let mut idx = MemoryInvertedIndex::new(uqa_analysis::standard_analyzer("english"));
    let idx_analyzer = Analyzer::new(
        Tokenizer::Whitespace,
        vec![TokenFilter::Lowercase],
        Vec::new(),
    );
    let mut syn = BTreeMap::new();
    syn.insert("automobile".to_string(), vec!["car".to_string()]);
    let search_analyzer = Analyzer::new(
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
    idx.set_field_analyzer("body", idx_analyzer, AnalyzerPhase::Index)
        .unwrap();
    idx.set_field_analyzer("body", search_analyzer, AnalyzerPhase::Search)
        .unwrap();
    idx.add_document(1, fields(&[("body", "used car for sale")]))
        .unwrap();
    idx.add_document(2, fields(&[("body", "new bike for sale")]))
        .unwrap();

    let mut ctx = ExecutionContext::new();
    ctx.inverted_index = Some(idx.snapshot().unwrap());
    let op = TermOperator::new("automobile", "body");
    let result = op.execute(&ctx).unwrap();
    let doc_ids: Vec<u64> = result.iter().map(|e| e.doc_id).collect();
    assert!(doc_ids.contains(&1));
    assert!(!doc_ids.contains(&2));
}

#[test]
fn no_synonym_single_token() {
    let mut idx = MemoryInvertedIndex::new(uqa_analysis::standard_analyzer("english"));
    idx.add_document(1, fields(&[("body", "used car for sale")]))
        .unwrap();

    let mut ctx = ExecutionContext::new();
    ctx.inverted_index = Some(idx.snapshot().unwrap());
    let op = TermOperator::new("car", "body");
    let result = op.execute(&ctx).unwrap();
    let doc_ids: Vec<u64> = result.iter().map(|e| e.doc_id).collect();
    assert!(doc_ids.contains(&1));
}

#[test]
fn invalid_search_analyzer_is_returned_as_operator_error() {
    let mut index = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    index
        .add_document(1, fields(&[("body", "searchable text")]))
        .unwrap();
    index
        .set_field_analyzer(
            "body",
            Analyzer::new(
                Tokenizer::Pattern {
                    pattern: "[".into(),
                },
                Vec::new(),
                Vec::new(),
            ),
            AnalyzerPhase::Search,
        )
        .unwrap();

    let context = ExecutionContext::new().with_inverted_index(index.snapshot().unwrap());
    let error = TermOperator::new("searchable", "body")
        .execute(&context)
        .unwrap_err();
    assert!(matches!(error, StorageBackendError::Analysis(_)));
}
