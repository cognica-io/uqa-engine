//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inverted index abstraction and an in-memory implementation.
//!
//! The index maps `(field, term)` keys to posting lists, tracks per-field
//! token lengths and corpus statistics, and indexes documents by running
//! an [`Analyzer`] over each field's text.

use std::collections::{BTreeMap, BTreeSet};

use uqa_analysis::Analyzer;
use uqa_core::{DocId, FieldName, IndexStats, Payload, PostingEntry, PostingList};

pub trait InvertedIndex: Send + Sync {
    fn analyzer(&self) -> &Analyzer;

    fn add_document(&mut self, doc_id: DocId, fields: BTreeMap<FieldName, String>);

    fn remove_document(&mut self, doc_id: DocId);

    fn clear(&mut self);

    fn get_posting_list(&self, field: &str, term: &str) -> PostingList;

    fn doc_freq(&self, field: &str, term: &str) -> u64;

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> u64;

    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> u64;

    fn doc_count(&self) -> u64;

    fn total_field_length(&self, field: &str) -> u64;

    /// Fully-populated [`IndexStats`] snapshot for the cost model and
    /// scoring layer. Implementations may cache this between mutations.
    fn stats(&self) -> IndexStats;
}

#[derive(Debug, Clone)]
pub struct MemoryInvertedIndex {
    analyzer: Analyzer,
    /// `(field, term) -> doc_id -> entry (positions inside the doc)`
    index: BTreeMap<(FieldName, String), BTreeMap<DocId, PostingEntry>>,
    /// Reverse index for `remove_document` so we touch only relevant
    /// `(field, term)` posting maps instead of scanning the whole index.
    doc_terms: BTreeMap<DocId, BTreeSet<(FieldName, String)>>,
    /// Per-document field length in tokens.
    doc_lengths: BTreeMap<DocId, BTreeMap<FieldName, u64>>,
    /// Sum of field lengths across all docs, per field.
    total_length: BTreeMap<FieldName, u64>,
    doc_count: u64,
}

impl MemoryInvertedIndex {
    pub fn new(analyzer: Analyzer) -> Self {
        Self {
            analyzer,
            index: BTreeMap::new(),
            doc_terms: BTreeMap::new(),
            doc_lengths: BTreeMap::new(),
            total_length: BTreeMap::new(),
            doc_count: 0,
        }
    }
}

impl InvertedIndex for MemoryInvertedIndex {
    fn analyzer(&self) -> &Analyzer {
        &self.analyzer
    }

    fn add_document(&mut self, doc_id: DocId, fields: BTreeMap<FieldName, String>) {
        // Replacing an existing doc: remove its old postings first so the
        // length accumulators stay consistent.
        if self.doc_terms.contains_key(&doc_id) {
            self.remove_document(doc_id);
        }
        self.doc_count += 1;
        let mut per_doc_lengths: BTreeMap<FieldName, u64> = BTreeMap::new();
        let mut term_set: BTreeSet<(FieldName, String)> = BTreeSet::new();

        for (field, text) in fields {
            let tokens = self.analyzer.analyze(&text);
            let length = tokens.len() as u64;
            per_doc_lengths.insert(field.clone(), length);
            *self.total_length.entry(field.clone()).or_insert(0) += length;

            // Group token positions by term.
            let mut term_positions: BTreeMap<String, Vec<u32>> = BTreeMap::new();
            for (pos, token) in tokens.into_iter().enumerate() {
                term_positions.entry(token).or_default().push(pos as u32);
            }

            for (term, mut positions) in term_positions {
                positions.sort_unstable();
                positions.dedup();
                let entry = PostingEntry::new(
                    doc_id,
                    Payload {
                        positions,
                        score: 0.0,
                        fields: BTreeMap::new(),
                    },
                );
                let key = (field.clone(), term);
                self.index.entry(key.clone()).or_default().insert(doc_id, entry);
                term_set.insert(key);
            }
        }

        self.doc_lengths.insert(doc_id, per_doc_lengths);
        self.doc_terms.insert(doc_id, term_set);
    }

    fn remove_document(&mut self, doc_id: DocId) {
        let Some(keys) = self.doc_terms.remove(&doc_id) else {
            return;
        };
        for key in keys {
            if let Some(inner) = self.index.get_mut(&key) {
                inner.remove(&doc_id);
                if inner.is_empty() {
                    self.index.remove(&key);
                }
            }
        }
        if let Some(lengths) = self.doc_lengths.remove(&doc_id) {
            for (field, length) in lengths {
                if let Some(total) = self.total_length.get_mut(&field) {
                    *total = total.saturating_sub(length);
                }
            }
        }
        if self.doc_count > 0 {
            self.doc_count -= 1;
        }
    }

    fn clear(&mut self) {
        self.index.clear();
        self.doc_terms.clear();
        self.doc_lengths.clear();
        self.total_length.clear();
        self.doc_count = 0;
    }

    fn get_posting_list(&self, field: &str, term: &str) -> PostingList {
        let key = (field.to_string(), term.to_string());
        let Some(inner) = self.index.get(&key) else {
            return PostingList::new();
        };
        let entries: Vec<PostingEntry> = inner.values().cloned().collect();
        PostingList::from_sorted_unchecked(entries)
    }

    fn doc_freq(&self, field: &str, term: &str) -> u64 {
        let key = (field.to_string(), term.to_string());
        self.index.get(&key).map_or(0, |inner| inner.len() as u64)
    }

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> u64 {
        self.doc_lengths
            .get(&doc_id)
            .and_then(|lengths| lengths.get(field).copied())
            .unwrap_or(0)
    }

    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> u64 {
        let key = (field.to_string(), term.to_string());
        self.index
            .get(&key)
            .and_then(|inner| inner.get(&doc_id))
            .map_or(0, |e| e.payload.positions.len() as u64)
    }

    fn doc_count(&self) -> u64 {
        self.doc_count
    }

    fn total_field_length(&self, field: &str) -> u64 {
        self.total_length.get(field).copied().unwrap_or(0)
    }

    fn stats(&self) -> IndexStats {
        let mut s = IndexStats::default();
        s.total_docs = self.doc_count;
        if self.doc_count > 0 {
            let total: u64 = self.total_length.values().sum();
            s.avg_doc_length = total as f64 / self.doc_count as f64;
        }
        for ((field, term), inner) in &self.index {
            s.set_doc_freq(field.clone(), term.clone(), inner.len() as u64);
        }
        s
    }
}

#[cfg(test)]
mod tests {
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
        idx.add_document(1, fields([("title", "The Rust Programming Language")]));
        idx.add_document(2, fields([("title", "Programming with Rust")]));

        let pl = idx.get_posting_list("title", "rust");
        let docs: Vec<_> = pl.doc_ids().collect();
        assert_eq!(docs, vec![1, 2]);

        // standard analyzer stems "programming" -> "program"
        let pl2 = idx.get_posting_list("title", "program");
        let docs2: Vec<_> = pl2.doc_ids().collect();
        assert_eq!(docs2, vec![1, 2]);
    }

    #[test]
    fn doc_freq_counts_documents() {
        let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
        idx.add_document(1, fields([("title", "rust")]));
        idx.add_document(2, fields([("title", "rust rust rust")]));
        idx.add_document(3, fields([("title", "go")]));

        assert_eq!(idx.doc_freq("title", "rust"), 2);
        assert_eq!(idx.doc_freq("title", "go"), 1);
        assert_eq!(idx.doc_freq("title", "java"), 0);
    }

    #[test]
    fn term_freq_counts_positions() {
        let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
        idx.add_document(1, fields([("title", "rust rust rust")]));
        // After standard analyzer: ["rust", "rust", "rust"]
        assert_eq!(idx.get_term_freq(1, "title", "rust"), 3);
    }

    #[test]
    fn doc_length_tracks_token_count() {
        let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
        // standard analyzer drops "the" / "is" stop words
        idx.add_document(1, fields([("title", "the rust language is fast")]));
        // Remaining tokens: ["rust", "languag", "fast"] -> 3
        assert_eq!(idx.get_doc_length(1, "title"), 3);
    }

    #[test]
    fn remove_document_clears_postings_and_lengths() {
        let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
        idx.add_document(1, fields([("title", "rust")]));
        idx.add_document(2, fields([("title", "rust")]));
        idx.remove_document(1);

        assert_eq!(idx.doc_freq("title", "rust"), 1);
        assert_eq!(idx.get_doc_length(1, "title"), 0);
        assert_eq!(idx.doc_count(), 1);
    }

    #[test]
    fn replacing_doc_replaces_postings() {
        let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
        idx.add_document(1, fields([("title", "rust")]));
        idx.add_document(1, fields([("title", "go")]));

        assert_eq!(idx.doc_freq("title", "rust"), 0);
        assert_eq!(idx.doc_freq("title", "go"), 1);
        assert_eq!(idx.doc_count(), 1);
    }

    #[test]
    fn stats_avg_doc_length_correct() {
        let mut idx = MemoryInvertedIndex::new(standard_analyzer("english"));
        idx.add_document(1, fields([("title", "rust language")]));
        idx.add_document(2, fields([("title", "rust")]));
        let s = idx.stats();
        assert_eq!(s.total_docs, 2);
        // 2 + 1 = 3 tokens / 2 docs = 1.5
        assert!((s.avg_doc_length - 1.5).abs() < 1e-9);
        assert_eq!(s.doc_freq("title", "rust"), 2);
    }
}
