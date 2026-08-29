//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    counter_error, usize_to_u64, Analyzer, Arc, BTreeMap, BlockMaxScorer, DocId, FieldName,
    IndexStats, PostingEntry, PostingList, StorageBackendError, StorageBackendResult,
};
use crate::clustered_postings::{MaterializedPostingCursor, PostingCursor, PostingScore};

/// Which side of the index/search pipeline a field analyzer applies to.
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

    /// Add or replace several documents in input order. The default preserves the point-mutation contract for custom backends; transactional persistent backends can override this to make the call atomic and coalesce writes that share physical posting clusters.
    fn try_add_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        for (doc_id, fields) in documents {
            self.try_add_document(doc_id, fields)?;
        }
        Ok(())
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

    /// Open a doc-id ordered score cursor for one term.
    ///
    /// The cursor carries term frequency and document length directly so
    /// ranking does not need positional payloads or per-document length
    /// lookups. Persistent backends override this with lazy clustered
    /// cursors; the default preserves compatibility for custom backends.
    fn posting_cursor(
        &self,
        field: &str,
        term: &str,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        let posting_list = self.get_posting_list(field, term)?;
        let mut entries = Vec::with_capacity(posting_list.len());
        for posting in posting_list {
            let term_freq = usize_to_u64(posting.payload.positions.len().max(1), "term frequency")?;
            entries.push(PostingScore {
                doc_id: posting.doc_id,
                term_freq,
                doc_length: self.get_doc_length(posting.doc_id, field)?.max(term_freq),
            });
        }
        Ok(Box::new(MaterializedPostingCursor::new(entries)?))
    }

    fn posting_cursors_bulk(
        &self,
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<Box<dyn PostingCursor>>> {
        terms
            .iter()
            .map(|term| self.posting_cursor(field, term))
            .collect()
    }

    /// Persist scorer-specific block maxima for every term in `field`.
    ///
    /// Backends that do not provide durable auxiliary indexes return `false`.
    /// The fingerprint must include every scorer and corpus statistic that can
    /// affect a term contribution; reads only expose rows with an exact match.
    fn rebuild_persisted_block_max(
        &mut self,
        _field: &str,
        _scorer: &dyn BlockMaxScorer,
        _scorer_fingerprint: &str,
    ) -> StorageBackendResult<bool> {
        Ok(false)
    }

    /// Load scorer-versioned block maxima for one posting list. `None` means
    /// the backend has no complete, valid materialization for this scorer.
    fn persisted_block_max_scores(
        &self,
        _field: &str,
        _term: &str,
        _scorer_fingerprint: &str,
    ) -> StorageBackendResult<Option<Vec<f64>>> {
        Ok(None)
    }

    /// Load scorer-versioned block maxima for several terms while preserving input order; persistent backends override this to avoid one storage round trip per term.
    fn persisted_block_max_scores_bulk(
        &self,
        field: &str,
        terms: &[String],
        scorer_fingerprint: &str,
    ) -> StorageBackendResult<Vec<Option<Vec<f64>>>> {
        terms
            .iter()
            .map(|term| self.persisted_block_max_scores(field, term, scorer_fingerprint))
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

    // -- Extended inverted-index surface ---

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

    /// Search-time analyzer for `field`; falls back to the index-time analyzer,
    /// then to the default.
    fn get_search_analyzer(&self, field: &str) -> Analyzer {
        self.get_field_analyzer(field)
    }
}
