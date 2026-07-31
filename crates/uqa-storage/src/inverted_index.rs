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

use crate::backend::{StorageBackendError, StorageBackendResult};

fn counter_error(context: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("inverted-index {context} overflow or corruption"))
}

fn usize_to_u64(value: usize, context: &str) -> StorageBackendResult<u64> {
    u64::try_from(value).map_err(|_| counter_error(context))
}

fn checked_sum_u64(
    values: impl IntoIterator<Item = u64>,
    context: &str,
) -> StorageBackendResult<u64> {
    values.into_iter().try_fold(0_u64, |total, value| {
        total
            .checked_add(value)
            .ok_or_else(|| counter_error(context))
    })
}

pub(crate) fn validate_token_position_count(token_count: u64) -> StorageBackendResult<()> {
    // Positions are zero-based, so a stream containing u32::MAX + 1 tokens
    // still has a representable final position (u32::MAX).
    if token_count > u64::from(u32::MAX) + 1 {
        return Err(StorageBackendError::Other(
            "document token positions exceed the u32 index format".into(),
        ));
    }
    Ok(())
}

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

    fn add_document(
        &mut self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()>;

    fn try_add_document(
        &mut self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        self.add_document(doc_id, fields)
    }

    fn remove_document(&mut self, doc_id: DocId) -> StorageBackendResult<()>;

    fn try_remove_document(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.remove_document(doc_id)
    }

    fn clear(&mut self) -> StorageBackendResult<()>;

    fn try_clear(&mut self) -> StorageBackendResult<()> {
        self.clear()
    }

    fn try_rebuild_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        self.try_clear()?;
        for (doc_id, fields) in documents {
            if !fields.is_empty() {
                self.try_add_document(doc_id, fields)?;
            }
        }
        Ok(())
    }

    fn get_posting_list(&self, field: &str, term: &str) -> StorageBackendResult<PostingList>;

    fn get_posting_lists_bulk(
        &self,
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<PostingList>> {
        terms
            .iter()
            .map(|term| self.get_posting_list(field, term))
            .collect()
    }

    /// Visit every posting entry for `(field, term)` in ascending
    /// doc-id order without handing out an owned list.
    ///
    /// [`InvertedIndex::get_posting_list`] deep-copies each entry's
    /// payload (positions vector included), which costs one heap
    /// allocation per matching document. Read-only scoring walks use
    /// this instead; backends whose postings already live in memory
    /// override it to iterate in place.
    fn for_each_posting(
        &self,
        field: &str,
        term: &str,
        visit: &mut dyn FnMut(&PostingEntry),
    ) -> StorageBackendResult<()> {
        for entry in &self.get_posting_list(field, term)? {
            visit(entry);
        }
        Ok(())
    }

    /// Visit `(doc_id, term_frequency)` pairs without requiring callers to
    /// materialize or decode payload details they do not use. The default
    /// keeps every backend compatible through the posting-list contract;
    /// persistent backends can stream compact frequency projections.
    fn for_each_term_freq(
        &self,
        field: &str,
        term: &str,
        visit: &mut dyn FnMut(DocId, u64),
    ) -> StorageBackendResult<()> {
        for entry in &self.get_posting_list(field, term)? {
            visit(
                entry.doc_id,
                usize_to_u64(entry.payload.positions.len(), "term frequency")?,
            );
        }
        Ok(())
    }

    fn doc_freq(&self, field: &str, term: &str) -> StorageBackendResult<u64>;

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> StorageBackendResult<u64>;

    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> StorageBackendResult<u64>;

    fn doc_count(&self) -> StorageBackendResult<u64>;

    fn total_field_length(&self, field: &str) -> StorageBackendResult<u64>;

    /// Number of documents that have indexed content for `field`.
    fn field_doc_count(&self, field: &str) -> StorageBackendResult<u64> {
        self.doc_length_count(Some(field))
    }

    /// Field-specific statistics for BM25 scoring.
    ///
    /// BM25 length normalization and IDF collection size are defined for
    /// one field. Reusing table-wide totals mixes unrelated field lengths
    /// and produces scores that cannot match a field-scoped BM25 scorer.
    fn field_stats(&self, field: &str) -> StorageBackendResult<IndexStats> {
        let mut stats = self.stats()?;
        let field_docs = self.field_doc_count(field)?;
        stats.total_docs = field_docs;
        stats.avg_doc_length = if field_docs > 0 {
            self.total_field_length(field)? as f64 / field_docs as f64
        } else {
            0.0
        };
        Ok(stats)
    }

    /// [`InvertedIndex::field_stats`] without the vocabulary-wide
    /// document-frequency map.
    ///
    /// Query execution that already knows its terms' document
    /// frequencies (it read them off the posting lists) only needs the
    /// field's document count and average length; copying the whole
    /// term dictionary per query is O(vocabulary) for nothing.
    fn field_stats_scalar(&self, field: &str) -> StorageBackendResult<IndexStats> {
        let mut stats = IndexStats::default();
        let field_docs = self.field_doc_count(field)?;
        stats.total_docs = field_docs;
        stats.avg_doc_length = if field_docs > 0 {
            self.total_field_length(field)? as f64 / field_docs as f64
        } else {
            0.0
        };
        Ok(stats)
    }

    /// Sorted unique indexed terms for `field`.
    ///
    /// Backends implement this from their term dictionary rather than by
    /// re-analyzing stored documents. This is the source used by Bayesian
    /// calibration reservoir sampling.
    fn vocabulary_terms(&self, _field: &str) -> StorageBackendResult<Vec<String>> {
        Ok(Vec::new())
    }

    /// Fully-populated [`IndexStats`] snapshot for the cost model and
    /// scoring layer. Implementations may cache this between mutations.
    fn stats(&self) -> StorageBackendResult<IndexStats>;

    /// Number of posting rows. With `field = Some(..)`, limits the count
    /// to one indexed field.
    fn posting_count(&self, _field: Option<&str>) -> StorageBackendResult<u64> {
        Ok(0)
    }

    /// Number of `(doc_id, field)` length rows. With `field = Some(..)`,
    /// this is the number of documents indexed for that field.
    fn doc_length_count(&self, _field: Option<&str>) -> StorageBackendResult<u64> {
        Ok(0)
    }

    /// Number of distinct indexed terms. With `field = Some(..)`, limits
    /// the count to one indexed field.
    fn term_count(&self, _field: Option<&str>) -> StorageBackendResult<u64> {
        Ok(0)
    }

    /// Read-only handle suitable for an `ExecutionContext`.
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>>;

    /// Independent writable copy used to restore an in-memory engine
    /// transaction without reconstructing analyzer state from documents.
    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn InvertedIndex>> {
        Err(StorageBackendError::Other(
            "writable inverted-index snapshots are not supported by this backend".into(),
        ))
    }

    // -- Mirrors `uqa.storage.abc.InvertedIndex` extended surface ---

    /// Names of every field with at least one indexed document.
    /// Default implementation walks the [`IndexStats`] snapshot's
    /// total-length map. Backends with a richer schema can override.
    fn field_names(&self) -> StorageBackendResult<Vec<FieldName>> {
        Ok(Vec::new())
    }

    /// Posting list for `term` across every indexed field, unioned
    /// together. Default implementation sums per-field posting lists
    /// via [`PostingList::merge_union`].
    fn get_posting_list_any_field(&self, term: &str) -> StorageBackendResult<PostingList> {
        let mut result = PostingList::new();
        for field in self.field_names()? {
            let pl = self.get_posting_list(&field, term)?;
            result = result.merge_union(&pl);
        }
        Ok(result)
    }

    /// Document frequency of `term` across every indexed field.
    fn doc_freq_any_field(&self, term: &str) -> StorageBackendResult<u64> {
        let mut total = 0_u64;
        for field in self.field_names()? {
            total = total
                .checked_add(self.doc_freq(&field, term)?)
                .ok_or_else(|| counter_error("document frequency"))?;
        }
        Ok(total)
    }

    /// Sum of all per-field token lengths for a single doc.
    fn get_total_doc_length(&self, doc_id: DocId) -> StorageBackendResult<u64> {
        let mut total = 0_u64;
        for field in self.field_names()? {
            total = total
                .checked_add(self.get_doc_length(doc_id, &field)?)
                .ok_or_else(|| counter_error("document length"))?;
        }
        Ok(total)
    }

    /// Bulk doc-length lookup. Default falls back to per-id calls.
    fn get_doc_lengths_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
    ) -> StorageBackendResult<BTreeMap<DocId, u64>> {
        let mut out = BTreeMap::new();
        for doc_id in doc_ids {
            out.insert(*doc_id, self.get_doc_length(*doc_id, field)?);
        }
        Ok(out)
    }

    /// Bulk term-frequency lookup. Default falls back to per-id calls.
    fn get_term_freqs_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
        term: &str,
    ) -> StorageBackendResult<BTreeMap<DocId, u64>> {
        let mut out = BTreeMap::new();
        for doc_id in doc_ids {
            out.insert(*doc_id, self.get_term_freq(*doc_id, field, term)?);
        }
        Ok(out)
    }

    /// Fetch the document length and one term frequency per query term for
    /// every requested document. Results stay aligned with `doc_ids`.
    /// Persistent backends override this to collapse the scoring loop's
    /// per-document point reads into a small number of set-oriented queries.
    fn get_scoring_inputs_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<(u64, Vec<u64>)>> {
        let mut out = Vec::with_capacity(doc_ids.len());
        for doc_id in doc_ids {
            let mut term_freqs = Vec::with_capacity(terms.len());
            for term in terms {
                term_freqs.push(self.get_term_freq(*doc_id, field, term)?);
            }
            out.push((self.get_doc_length(*doc_id, field)?, term_freqs));
        }
        Ok(out)
    }

    /// Total term frequency for a doc summed across every indexed
    /// field.
    fn get_total_term_freq(&self, doc_id: DocId, term: &str) -> StorageBackendResult<u64> {
        let mut total = 0_u64;
        for field in self.field_names()? {
            total = total
                .checked_add(self.get_term_freq(doc_id, &field, term)?)
                .ok_or_else(|| counter_error("term frequency"))?;
        }
        Ok(total)
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

    /// Remove every per-field analyzer override for `field`.  This is the
    /// inverse of `set_field_analyzer(..., Both)` and is required when the
    /// final logical FTS index for a field is dropped.  The default errors so
    /// a backend cannot silently retain stale analysis behavior.
    fn remove_field_analyzers(&mut self, _field: &str) -> Result<(), String> {
        Err("remove_field_analyzers not supported by this InvertedIndex backend".into())
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
    /// Number of documents with indexed content per field, maintained
    /// incrementally so per-query BM25 statistics never walk
    /// `doc_lengths` (O(corpus) at query time otherwise).
    field_doc_counts: BTreeMap<FieldName, u64>,
    doc_count: u64,
    /// Per-field analyzer override applied at index time. Falls back
    /// to [`MemoryInvertedIndex::analyzer`] when no entry exists.
    index_field_analyzers: BTreeMap<FieldName, Analyzer>,
    /// Per-field analyzer override applied at search time (e.g. for
    /// synonym expansion that must not be persisted into the postings).
    search_field_analyzers: BTreeMap<FieldName, Analyzer>,
}

type PostingKey = (FieldName, String);

struct StagedMemoryDocument {
    lengths: BTreeMap<FieldName, u64>,
    terms: BTreeSet<PostingKey>,
    postings: Vec<(PostingKey, PostingEntry)>,
}

struct MemoryReplacementPlan {
    old_terms: BTreeSet<PostingKey>,
    next_doc_count: u64,
    field_counters: BTreeMap<FieldName, (u64, u64)>,
}

impl MemoryInvertedIndex {
    pub fn new(analyzer: Analyzer) -> Self {
        Self {
            analyzer,
            index: BTreeMap::new(),
            doc_terms: BTreeMap::new(),
            doc_lengths: BTreeMap::new(),
            total_length: BTreeMap::new(),
            field_doc_counts: BTreeMap::new(),
            doc_count: 0,
            index_field_analyzers: BTreeMap::new(),
            search_field_analyzers: BTreeMap::new(),
        }
    }

    fn stage_document(
        &self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<StagedMemoryDocument> {
        let mut lengths = BTreeMap::new();
        let mut terms = BTreeSet::new();
        let mut postings = Vec::new();
        for (field, text) in fields {
            let analyzer = self
                .index_field_analyzers
                .get(&field)
                .unwrap_or(&self.analyzer);
            let tokens = analyzer.analyze(&text)?;
            let length = usize_to_u64(tokens.len(), "document token count")?;
            validate_token_position_count(length)?;
            lengths.insert(field.clone(), length);

            let mut term_positions: BTreeMap<String, Vec<u32>> = BTreeMap::new();
            for (position, token) in tokens.into_iter().enumerate() {
                term_positions.entry(token).or_default().push(
                    u32::try_from(position)
                        .map_err(|_| counter_error("document token position"))?,
                );
            }
            for (term, mut positions) in term_positions {
                positions.sort_unstable();
                positions.dedup();
                let key = (field.clone(), term);
                terms.insert(key.clone());
                postings.push((
                    key,
                    PostingEntry::new(
                        doc_id,
                        Payload {
                            positions,
                            score: 0.0,
                            fields: BTreeMap::new(),
                        },
                    ),
                ));
            }
        }
        Ok(StagedMemoryDocument {
            lengths,
            terms,
            postings,
        })
    }

    fn plan_replacement(
        &self,
        doc_id: DocId,
        new_lengths: &BTreeMap<FieldName, u64>,
    ) -> StorageBackendResult<MemoryReplacementPlan> {
        let has_terms = self.doc_terms.contains_key(&doc_id);
        if has_terms != self.doc_lengths.contains_key(&doc_id) {
            return Err(StorageBackendError::Other(format!(
                "inverted-index document {doc_id} has inconsistent reverse-index state"
            )));
        }
        let old_terms = self.doc_terms.get(&doc_id).cloned().unwrap_or_default();
        let old_lengths = self.doc_lengths.get(&doc_id).cloned().unwrap_or_default();
        let next_doc_count = self
            .doc_count
            .checked_sub(u64::from(has_terms))
            .ok_or_else(|| counter_error("document count"))?
            .checked_add(u64::from(!new_lengths.is_empty()))
            .ok_or_else(|| counter_error("document count"))?;
        for key in &old_terms {
            if !self
                .index
                .get(key)
                .is_some_and(|postings| postings.contains_key(&doc_id))
            {
                return Err(StorageBackendError::Other(format!(
                    "inverted-index document {doc_id} references a missing posting"
                )));
            }
        }

        let mut affected_fields = BTreeSet::new();
        affected_fields.extend(old_lengths.keys().cloned());
        affected_fields.extend(new_lengths.keys().cloned());
        let mut field_counters = BTreeMap::new();
        for field in affected_fields {
            let old_length = old_lengths.get(&field).copied().unwrap_or(0);
            let new_length = new_lengths.get(&field).copied().unwrap_or(0);
            let total = self
                .total_length
                .get(&field)
                .copied()
                .unwrap_or(0)
                .checked_sub(old_length)
                .ok_or_else(|| counter_error("total field length"))?
                .checked_add(new_length)
                .ok_or_else(|| counter_error("total field length"))?;
            let field_docs = self
                .field_doc_counts
                .get(&field)
                .copied()
                .unwrap_or(0)
                .checked_sub(u64::from(old_lengths.contains_key(&field)))
                .ok_or_else(|| counter_error("field document count"))?
                .checked_add(u64::from(new_lengths.contains_key(&field)))
                .ok_or_else(|| counter_error("field document count"))?;
            field_counters.insert(field, (total, field_docs));
        }
        Ok(MemoryReplacementPlan {
            old_terms,
            next_doc_count,
            field_counters,
        })
    }

    fn apply_replacement(
        &mut self,
        doc_id: DocId,
        staged: StagedMemoryDocument,
        plan: MemoryReplacementPlan,
    ) -> StorageBackendResult<()> {
        for key in plan.old_terms {
            let postings = self.index.get_mut(&key).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "inverted-index document {doc_id} lost a validated posting before replacement"
                ))
            })?;
            postings.remove(&doc_id);
            if postings.is_empty() {
                self.index.remove(&key);
            }
        }
        self.doc_lengths.remove(&doc_id);
        self.doc_terms.remove(&doc_id);
        for (field, (total, field_docs)) in plan.field_counters {
            if field_docs == 0 {
                self.total_length.remove(&field);
                self.field_doc_counts.remove(&field);
            } else {
                self.total_length.insert(field.clone(), total);
                self.field_doc_counts.insert(field, field_docs);
            }
        }
        for (key, entry) in staged.postings {
            self.index.entry(key).or_default().insert(doc_id, entry);
        }
        self.doc_count = plan.next_doc_count;
        if !staged.lengths.is_empty() {
            self.doc_lengths.insert(doc_id, staged.lengths);
            self.doc_terms.insert(doc_id, staged.terms);
        }
        Ok(())
    }
}

impl InvertedIndex for MemoryInvertedIndex {
    fn analyzer(&self) -> &Analyzer {
        &self.analyzer
    }

    fn add_document(
        &mut self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        // Analysis can fail because analyzer definitions are persisted and may
        // contain an invalid regex/gram range, or because a synonym file was
        // removed after registration. Stage every field before touching index
        // state so a failed replacement leaves the prior document intact.
        let staged = self.stage_document(doc_id, fields)?;
        let plan = self.plan_replacement(doc_id, &staged.lengths)?;
        self.apply_replacement(doc_id, staged, plan)
    }

    fn remove_document(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let Some(keys) = self.doc_terms.get(&doc_id).cloned() else {
            if self.doc_lengths.contains_key(&doc_id) {
                return Err(StorageBackendError::Other(format!(
                    "inverted-index document {doc_id} has lengths but no reverse postings"
                )));
            }
            return Ok(());
        };
        let lengths = self.doc_lengths.get(&doc_id).cloned().ok_or_else(|| {
            StorageBackendError::Other(format!(
                "inverted-index document {doc_id} has reverse postings but no lengths"
            ))
        })?;
        let next_doc_count = self
            .doc_count
            .checked_sub(1)
            .ok_or_else(|| counter_error("document count"))?;
        for key in &keys {
            if !self
                .index
                .get(key)
                .is_some_and(|postings| postings.contains_key(&doc_id))
            {
                return Err(StorageBackendError::Other(format!(
                    "inverted-index document {doc_id} references a missing posting"
                )));
            }
        }
        let mut next_field_counters = BTreeMap::new();
        for (field, length) in &lengths {
            let total = self
                .total_length
                .get(field)
                .copied()
                .unwrap_or(0)
                .checked_sub(*length)
                .ok_or_else(|| counter_error("total field length"))?;
            let field_docs = self
                .field_doc_counts
                .get(field)
                .copied()
                .unwrap_or(0)
                .checked_sub(1)
                .ok_or_else(|| counter_error("field document count"))?;
            next_field_counters.insert(field.clone(), (total, field_docs));
        }

        for key in keys {
            let inner = self.index.get_mut(&key).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "inverted-index document {doc_id} lost a validated posting before removal"
                ))
            })?;
            inner.remove(&doc_id);
            if inner.is_empty() {
                self.index.remove(&key);
            }
        }
        self.doc_terms.remove(&doc_id);
        self.doc_lengths.remove(&doc_id);
        for (field, (total, field_docs)) in next_field_counters {
            if field_docs == 0 {
                self.total_length.remove(&field);
                self.field_doc_counts.remove(&field);
            } else {
                self.total_length.insert(field.clone(), total);
                self.field_doc_counts.insert(field, field_docs);
            }
        }
        self.doc_count = next_doc_count;
        Ok(())
    }

    fn try_rebuild_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        let mut replacement = self.clone();
        replacement.clear()?;
        for (doc_id, fields) in documents {
            if !fields.is_empty() {
                replacement.add_document(doc_id, fields)?;
            }
        }
        *self = replacement;
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.index.clear();
        self.doc_terms.clear();
        self.doc_lengths.clear();
        self.total_length.clear();
        self.field_doc_counts.clear();
        self.doc_count = 0;
        Ok(())
    }

    fn get_posting_list(&self, field: &str, term: &str) -> StorageBackendResult<PostingList> {
        let key = (field.to_string(), term.to_string());
        let Some(inner) = self.index.get(&key) else {
            return Ok(PostingList::new());
        };
        let entries: Vec<PostingEntry> = inner.values().cloned().collect();
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn for_each_posting(
        &self,
        field: &str,
        term: &str,
        visit: &mut dyn FnMut(&PostingEntry),
    ) -> StorageBackendResult<()> {
        let key = (field.to_string(), term.to_string());
        if let Some(inner) = self.index.get(&key) {
            for entry in inner.values() {
                visit(entry);
            }
        }
        Ok(())
    }

    fn for_each_term_freq(
        &self,
        field: &str,
        term: &str,
        visit: &mut dyn FnMut(DocId, u64),
    ) -> StorageBackendResult<()> {
        let key = (field.to_string(), term.to_string());
        if let Some(inner) = self.index.get(&key) {
            for entry in inner.values() {
                visit(
                    entry.doc_id,
                    usize_to_u64(entry.payload.positions.len(), "term frequency")?,
                );
            }
        }
        Ok(())
    }

    fn doc_freq(&self, field: &str, term: &str) -> StorageBackendResult<u64> {
        let key = (field.to_string(), term.to_string());
        self.index.get(&key).map_or(Ok(0), |inner| {
            usize_to_u64(inner.len(), "document frequency")
        })
    }

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> StorageBackendResult<u64> {
        Ok(self
            .doc_lengths
            .get(&doc_id)
            .and_then(|lengths| lengths.get(field).copied())
            .unwrap_or(0))
    }

    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> StorageBackendResult<u64> {
        let key = (field.to_string(), term.to_string());
        Ok(self
            .index
            .get(&key)
            .and_then(|inner| inner.get(&doc_id))
            .map(|entry| usize_to_u64(entry.payload.positions.len(), "term frequency"))
            .transpose()?
            .unwrap_or(0))
    }

    fn doc_count(&self) -> StorageBackendResult<u64> {
        Ok(self.doc_count)
    }

    fn total_field_length(&self, field: &str) -> StorageBackendResult<u64> {
        Ok(self.total_length.get(field).copied().unwrap_or(0))
    }

    fn vocabulary_terms(&self, field: &str) -> StorageBackendResult<Vec<String>> {
        Ok(self
            .index
            .keys()
            .filter(|(indexed_field, _)| indexed_field == field)
            .map(|(_, term)| term.clone())
            .collect())
    }

    fn stats(&self) -> StorageBackendResult<IndexStats> {
        let mut s = IndexStats::default();
        s.total_docs = self.doc_count;
        if self.doc_count > 0 {
            let total =
                checked_sum_u64(self.total_length.values().copied(), "total document length")?;
            s.avg_doc_length = total as f64 / self.doc_count as f64;
        }
        for ((field, term), inner) in &self.index {
            s.set_doc_freq(
                field.clone(),
                term.clone(),
                usize_to_u64(inner.len(), "document frequency")?,
            );
        }
        Ok(s)
    }

    fn posting_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        checked_sum_u64(
            self.index
                .iter()
                .filter(|((f, _), _)| field.is_none_or(|target| f == target))
                .map(|(_, postings)| usize_to_u64(postings.len(), "posting count"))
                .collect::<StorageBackendResult<Vec<_>>>()?,
            "posting count",
        )
    }

    fn doc_length_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        Ok(match field {
            Some(target) => self.field_doc_counts.get(target).copied().unwrap_or(0),
            None => checked_sum_u64(
                self.field_doc_counts.values().copied(),
                "document-length row count",
            )?,
        })
    }

    fn term_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        usize_to_u64(
            self.index
                .keys()
                .filter(|(f, _)| field.is_none_or(|target| f == target))
                .map(|(_, term)| term)
                .collect::<BTreeSet<_>>()
                .len(),
            "term count",
        )
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        Ok(Arc::new(self.clone()))
    }

    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn InvertedIndex>> {
        Ok(Box::new(self.clone()))
    }

    fn field_names(&self) -> StorageBackendResult<Vec<FieldName>> {
        Ok(self.total_length.keys().cloned().collect())
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

    fn remove_field_analyzers(&mut self, field: &str) -> Result<(), String> {
        self.index_field_analyzers.remove(field);
        self.search_field_analyzers.remove(field);
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
}
