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
use std::sync::Arc;

use uqa_analysis::Analyzer;
use uqa_core::{DocId, FieldName, IndexStats, Payload, PostingEntry, PostingList};

/// Which side of the index/search pipeline a field analyzer applies to.
/// Matches UQA behavior for `set_field_analyzer(field, analyzer, phase=...)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyzerPhase {
    /// Run only when *adding* documents.
    Index,
    /// Run only when *querying* documents (e.g. through `TermOperator`).
    Search,
    /// Run on both phases (the default).
    Both,
}

impl AnalyzerPhase {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "index" => Ok(AnalyzerPhase::Index),
            "search" | "query" => Ok(AnalyzerPhase::Search),
            "both" => Ok(AnalyzerPhase::Both),
            _ => Err(format!("phase must be 'index'|'search'|'both', got `{s}`")),
        }
    }
}

impl std::str::FromStr for AnalyzerPhase {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

pub trait InvertedIndex: Send + Sync {
    fn analyzer(&self) -> &Analyzer;

    fn add_document(&mut self, doc_id: DocId, fields: BTreeMap<FieldName, String>);

    fn try_add_document(
        &mut self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> Result<(), String> {
        self.add_document(doc_id, fields);
        Ok(())
    }

    fn remove_document(&mut self, doc_id: DocId);

    fn try_remove_document(&mut self, doc_id: DocId) -> Result<(), String> {
        self.remove_document(doc_id);
        Ok(())
    }

    fn clear(&mut self);

    fn try_clear(&mut self) -> Result<(), String> {
        self.clear();
        Ok(())
    }

    fn try_rebuild_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> Result<(), String> {
        self.try_clear()?;
        for (doc_id, fields) in documents {
            if !fields.is_empty() {
                self.try_add_document(doc_id, fields)?;
            }
        }
        Ok(())
    }

    fn get_posting_list(&self, field: &str, term: &str) -> PostingList;

    fn get_posting_lists_bulk(&self, field: &str, terms: &[String]) -> Vec<PostingList> {
        terms
            .iter()
            .map(|term| self.get_posting_list(field, term))
            .collect()
    }

    fn doc_freq(&self, field: &str, term: &str) -> u64;

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> u64;

    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> u64;

    fn doc_count(&self) -> u64;

    fn total_field_length(&self, field: &str) -> u64;

    /// Number of documents that have indexed content for `field`.
    fn field_doc_count(&self, field: &str) -> u64 {
        self.doc_length_count(Some(field))
    }

    /// Field-specific statistics for BM25 scoring.
    ///
    /// BM25 length normalization and IDF collection size are defined for
    /// one field. Reusing table-wide totals mixes unrelated field lengths
    /// and produces scores that cannot match a field-scoped BM25 scorer.
    fn field_stats(&self, field: &str) -> IndexStats {
        let mut stats = self.stats();
        let field_docs = self.field_doc_count(field);
        stats.total_docs = field_docs;
        stats.avg_doc_length = if field_docs > 0 {
            self.total_field_length(field) as f64 / field_docs as f64
        } else {
            0.0
        };
        stats
    }

    /// Sorted unique indexed terms for `field`.
    ///
    /// Backends implement this from their term dictionary rather than by
    /// re-analyzing stored documents. This is the source used by Bayesian
    /// calibration reservoir sampling.
    fn vocabulary_terms(&self, _field: &str) -> Vec<String> {
        Vec::new()
    }

    /// Fully-populated [`IndexStats`] snapshot for the cost model and
    /// scoring layer. Implementations may cache this between mutations.
    fn stats(&self) -> IndexStats;

    /// Number of posting rows. With `field = Some(..)`, limits the count
    /// to one indexed field.
    fn posting_count(&self, _field: Option<&str>) -> u64 {
        0
    }

    /// Number of `(doc_id, field)` length rows. With `field = Some(..)`,
    /// this is the number of documents indexed for that field.
    fn doc_length_count(&self, _field: Option<&str>) -> u64 {
        0
    }

    /// Number of distinct indexed terms. With `field = Some(..)`, limits
    /// the count to one indexed field.
    fn term_count(&self, _field: Option<&str>) -> u64 {
        0
    }

    /// Read-only handle suitable for an `ExecutionContext`.
    fn snapshot(&self) -> Arc<dyn InvertedIndex>;

    // -- Mirrors `uqa.storage.abc.InvertedIndex` extended surface ---

    /// Names of every field with at least one indexed document.
    /// Default implementation walks the [`IndexStats`] snapshot's
    /// total-length map. Backends with a richer schema can override.
    fn field_names(&self) -> Vec<FieldName> {
        Vec::new()
    }

    /// Posting list for `term` across every indexed field, unioned
    /// together. Default implementation sums per-field posting lists
    /// via [`PostingList::union`].
    fn get_posting_list_any_field(&self, term: &str) -> PostingList {
        let mut result = PostingList::new();
        for field in self.field_names() {
            let pl = self.get_posting_list(&field, term);
            result = result.union(&pl);
        }
        result
    }

    /// Document frequency of `term` across every indexed field.
    fn doc_freq_any_field(&self, term: &str) -> u64 {
        let mut total = 0_u64;
        for field in self.field_names() {
            total = total.saturating_add(self.doc_freq(&field, term));
        }
        total
    }

    /// Sum of all per-field token lengths for a single doc.
    fn get_total_doc_length(&self, doc_id: DocId) -> u64 {
        let mut total = 0_u64;
        for field in self.field_names() {
            total = total.saturating_add(self.get_doc_length(doc_id, &field));
        }
        total
    }

    /// Bulk doc-length lookup. Default falls back to per-id calls.
    fn get_doc_lengths_bulk(&self, doc_ids: &[DocId], field: &str) -> BTreeMap<DocId, u64> {
        doc_ids
            .iter()
            .copied()
            .map(|d| (d, self.get_doc_length(d, field)))
            .collect()
    }

    /// Bulk term-frequency lookup. Default falls back to per-id calls.
    fn get_term_freqs_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
        term: &str,
    ) -> BTreeMap<DocId, u64> {
        doc_ids
            .iter()
            .copied()
            .map(|d| (d, self.get_term_freq(d, field, term)))
            .collect()
    }

    /// Total term frequency for a doc summed across every indexed
    /// field.
    fn get_total_term_freq(&self, doc_id: DocId, term: &str) -> u64 {
        let mut total = 0_u64;
        for field in self.field_names() {
            total = total.saturating_add(self.get_term_freq(doc_id, &field, term));
        }
        total
    }

    /// Bind an analyzer to a single field for the given phase.
    /// `Both` writes to both the index-side and search-side maps; the
    /// default impl errors so backends that don't support per-field
    /// analyzers fail loud rather than silently dropping the request.
    fn set_field_analyzer(
        &mut self,
        _field: &str,
        _analyzer: Analyzer,
        _phase: AnalyzerPhase,
    ) -> Result<(), String> {
        Err("set_field_analyzer not supported by this InvertedIndex backend".into())
    }

    /// Index-time analyzer for `field`; falls back to
    /// [`InvertedIndex::analyzer`] when no override is set.
    fn get_field_analyzer(&self, _field: &str) -> Analyzer {
        self.analyzer().clone()
    }

    /// Search-time analyzer for `field`; falls back to the index-time
    /// analyzer, then to the default. Matches UQA behavior for
    /// `get_search_analyzer`.
    fn get_search_analyzer(&self, field: &str) -> Analyzer {
        self.get_field_analyzer(field)
    }
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
    /// Per-field analyzer override applied at index time. Falls back
    /// to [`MemoryInvertedIndex::analyzer`] when no entry exists.
    index_field_analyzers: BTreeMap<FieldName, Analyzer>,
    /// Per-field analyzer override applied at search time (e.g. for
    /// synonym expansion that must not be persisted into the postings).
    search_field_analyzers: BTreeMap<FieldName, Analyzer>,
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
            index_field_analyzers: BTreeMap::new(),
            search_field_analyzers: BTreeMap::new(),
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
            let analyzer = self
                .index_field_analyzers
                .get(&field)
                .unwrap_or(&self.analyzer);
            let tokens = analyzer.analyze(&text);
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
                self.index
                    .entry(key.clone())
                    .or_default()
                    .insert(doc_id, entry);
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

    fn vocabulary_terms(&self, field: &str) -> Vec<String> {
        self.index
            .keys()
            .filter(|(indexed_field, _)| indexed_field == field)
            .map(|(_, term)| term.clone())
            .collect()
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

    fn posting_count(&self, field: Option<&str>) -> u64 {
        self.index
            .iter()
            .filter(|((f, _), _)| field.is_none_or(|target| f == target))
            .map(|(_, postings)| postings.len() as u64)
            .sum()
    }

    fn doc_length_count(&self, field: Option<&str>) -> u64 {
        match field {
            Some(target) => self
                .doc_lengths
                .values()
                .filter(|lengths| lengths.contains_key(target))
                .count() as u64,
            None => self
                .doc_lengths
                .values()
                .map(|lengths| lengths.len() as u64)
                .sum(),
        }
    }

    fn term_count(&self, field: Option<&str>) -> u64 {
        self.index
            .keys()
            .filter(|(f, _)| field.is_none_or(|target| f == target))
            .map(|(_, term)| term)
            .collect::<BTreeSet<_>>()
            .len() as u64
    }

    fn snapshot(&self) -> Arc<dyn InvertedIndex> {
        Arc::new(self.clone())
    }

    fn field_names(&self) -> Vec<FieldName> {
        self.total_length.keys().cloned().collect()
    }

    fn set_field_analyzer(
        &mut self,
        field: &str,
        analyzer: Analyzer,
        phase: AnalyzerPhase,
    ) -> Result<(), String> {
        match phase {
            AnalyzerPhase::Index => {
                self.index_field_analyzers
                    .insert(field.to_string(), analyzer);
            }
            AnalyzerPhase::Search => {
                self.search_field_analyzers
                    .insert(field.to_string(), analyzer);
            }
            AnalyzerPhase::Both => {
                self.index_field_analyzers
                    .insert(field.to_string(), analyzer.clone());
                self.search_field_analyzers
                    .insert(field.to_string(), analyzer);
            }
        }
        Ok(())
    }

    fn get_field_analyzer(&self, field: &str) -> Analyzer {
        self.index_field_analyzers
            .get(field)
            .cloned()
            .unwrap_or_else(|| self.analyzer.clone())
    }

    fn get_search_analyzer(&self, field: &str) -> Analyzer {
        if let Some(a) = self.search_field_analyzers.get(field) {
            return a.clone();
        }
        if let Some(a) = self.index_field_analyzers.get(field) {
            return a.clone();
        }
        self.analyzer.clone()
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
