//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_analysis::Analyzer;
use uqa_core::memory::Budgeted;
use uqa_core::{DocId, FieldName, IndexStats, PostingEntry, PostingList, TokenOccurrence};

use crate::block_max_index::BlockMaxScorer;
use crate::clustered_postings::{BudgetedPostingReadCursor, OccurrencePosting, PostingCursor};
use crate::inverted_index::{AnalyzerPhase, IndexedFieldMetadata, InvertedIndexChangeVisitor};
use crate::read_control::StorageReadControl;
use crate::{InvertedIndex, StorageBackendResult, TokenTermKey};

use super::{read_only_error, ReadOnlySnapshot};

impl InvertedIndex for ReadOnlySnapshot<dyn InvertedIndex> {
    fn source_rebuild_required(&self) -> StorageBackendResult<bool> {
        self.0.source_rebuild_required()
    }

    fn analyzer(&self) -> &Analyzer {
        self.0.analyzer()
    }

    fn add_document(
        &mut self,
        _doc_id: DocId,
        _fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn try_add_document(
        &mut self,
        _doc_id: DocId,
        _fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn try_add_documents(
        &mut self,
        _documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn try_add_documents_observed(
        &mut self,
        _documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
        _visit: &mut InvertedIndexChangeVisitor<'_>,
    ) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn try_remove_document_observed(
        &mut self,
        _doc_id: DocId,
        _visit: &mut InvertedIndexChangeVisitor<'_>,
    ) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn remove_document(&mut self, _doc_id: DocId) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn try_remove_document(&mut self, _doc_id: DocId) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn try_clear(&mut self) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn try_rebuild_documents(
        &mut self,
        _documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn get_posting_list(&self, field: &str, term: &str) -> StorageBackendResult<PostingList> {
        self.0.get_posting_list(field, term)
    }

    fn get_posting_list_key(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<PostingList> {
        self.0.get_posting_list_key(field, term)
    }

    fn posting_cursor_key(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        self.0.posting_cursor_key(field, term)
    }

    fn posting_read_cursor_key<'a>(
        &'a self,
        field: &'a str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Box<dyn crate::clustered_postings::PostingReadCursor + 'a>> {
        self.0.posting_read_cursor_key(field, term)
    }

    fn posting_read_cursor_key_budgeted<'a>(
        &'a self,
        field: &'a str,
        term: &'a TokenTermKey,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedPostingReadCursor<'a>> {
        self.0
            .posting_read_cursor_key_budgeted(field, term, control)
    }

    fn visit_score_clusters(
        &self,
        field: &str,
        term: &TokenTermKey,
        after: Option<u64>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut crate::clustered_postings::ScoreClusterVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.0
            .visit_score_clusters(field, term, after, limit, control, visit)
    }

    fn get_occurrences_budgeted(
        &self,
        doc_id: DocId,
        field: &str,
        term: &TokenTermKey,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Vec<TokenOccurrence>>> {
        self.0
            .get_occurrences_budgeted(doc_id, field, term, control)
    }

    fn get_occurrence_postings(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Vec<OccurrencePosting>> {
        self.0.get_occurrence_postings(field, term)
    }

    fn get_occurrences(
        &self,
        doc_id: DocId,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Vec<TokenOccurrence>> {
        self.0.get_occurrences(doc_id, field, term)
    }

    fn indexed_field_metadata(
        &self,
        doc_id: DocId,
        field: &str,
    ) -> StorageBackendResult<Option<IndexedFieldMetadata>> {
        self.0.indexed_field_metadata(doc_id, field)
    }

    fn doc_freq_key(&self, field: &str, term: &TokenTermKey) -> StorageBackendResult<u64> {
        self.0.doc_freq_key(field, term)
    }

    fn get_term_freq_key(
        &self,
        doc_id: DocId,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<u64> {
        self.0.get_term_freq_key(doc_id, field, term)
    }

    fn vocabulary_keys(&self, field: &str) -> StorageBackendResult<Vec<TokenTermKey>> {
        self.0.vocabulary_keys(field)
    }

    fn get_posting_lists_bulk(
        &self,
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<PostingList>> {
        self.0.get_posting_lists_bulk(field, terms)
    }

    fn posting_cursor(
        &self,
        field: &str,
        term: &str,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        self.0.posting_cursor(field, term)
    }

    fn posting_cursors_bulk(
        &self,
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<Box<dyn PostingCursor>>> {
        self.0.posting_cursors_bulk(field, terms)
    }

    fn posting_cursors_keys_bulk(
        &self,
        field: &str,
        terms: &[TokenTermKey],
    ) -> StorageBackendResult<Vec<Box<dyn PostingCursor>>> {
        self.0.posting_cursors_keys_bulk(field, terms)
    }

    fn get_posting_lists_keys_bulk(
        &self,
        field: &str,
        terms: &[TokenTermKey],
    ) -> StorageBackendResult<Vec<PostingList>> {
        self.0.get_posting_lists_keys_bulk(field, terms)
    }

    fn persisted_block_max_scores_keys_bulk(
        &self,
        field: &str,
        terms: &[TokenTermKey],
        scorer_fingerprint: &str,
    ) -> StorageBackendResult<Vec<Option<Vec<f64>>>> {
        self.0
            .persisted_block_max_scores_keys_bulk(field, terms, scorer_fingerprint)
    }

    fn get_scoring_inputs_keys_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
        terms: &[TokenTermKey],
    ) -> StorageBackendResult<Vec<(u64, Vec<u64>)>> {
        self.0.get_scoring_inputs_keys_bulk(doc_ids, field, terms)
    }

    fn rebuild_persisted_block_max(
        &mut self,
        _field: &str,
        _scorer: &dyn BlockMaxScorer,
        _scorer_fingerprint: &str,
    ) -> StorageBackendResult<bool> {
        Err(read_only_error())
    }

    fn persisted_block_max_scores(
        &self,
        field: &str,
        term: &str,
        scorer_fingerprint: &str,
    ) -> StorageBackendResult<Option<Vec<f64>>> {
        self.0
            .persisted_block_max_scores(field, term, scorer_fingerprint)
    }

    fn persisted_block_max_scores_bulk(
        &self,
        field: &str,
        terms: &[String],
        scorer_fingerprint: &str,
    ) -> StorageBackendResult<Vec<Option<Vec<f64>>>> {
        self.0
            .persisted_block_max_scores_bulk(field, terms, scorer_fingerprint)
    }

    fn for_each_posting(
        &self,
        field: &str,
        term: &str,
        visit: &mut dyn FnMut(&PostingEntry),
    ) -> StorageBackendResult<()> {
        self.0.for_each_posting(field, term, visit)
    }

    fn for_each_term_freq(
        &self,
        field: &str,
        term: &str,
        visit: &mut dyn FnMut(DocId, u64),
    ) -> StorageBackendResult<()> {
        self.0.for_each_term_freq(field, term, visit)
    }

    fn doc_freq(&self, field: &str, term: &str) -> StorageBackendResult<u64> {
        self.0.doc_freq(field, term)
    }

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> StorageBackendResult<u64> {
        self.0.get_doc_length(doc_id, field)
    }

    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> StorageBackendResult<u64> {
        self.0.get_term_freq(doc_id, field, term)
    }

    fn doc_count(&self) -> StorageBackendResult<u64> {
        self.0.doc_count()
    }

    fn total_field_length(&self, field: &str) -> StorageBackendResult<u64> {
        self.0.total_field_length(field)
    }

    fn field_doc_count(&self, field: &str) -> StorageBackendResult<u64> {
        self.0.field_doc_count(field)
    }

    fn field_stats(&self, field: &str) -> StorageBackendResult<IndexStats> {
        self.0.field_stats(field)
    }

    fn field_stats_scalar(&self, field: &str) -> StorageBackendResult<IndexStats> {
        self.0.field_stats_scalar(field)
    }

    fn field_stats_scalar_budgeted(
        &self,
        field: &str,
        control: &StorageReadControl,
    ) -> StorageBackendResult<IndexStats> {
        self.0.field_stats_scalar_budgeted(field, control)
    }

    fn vocabulary_terms(&self, field: &str) -> StorageBackendResult<Vec<String>> {
        self.0.vocabulary_terms(field)
    }

    fn stats(&self) -> StorageBackendResult<IndexStats> {
        self.0.stats()
    }

    fn posting_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        self.0.posting_count(field)
    }

    fn doc_length_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        self.0.doc_length_count(field)
    }

    fn term_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        self.0.term_count(field)
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        Ok(Arc::new(self.clone()))
    }

    fn field_names(&self) -> StorageBackendResult<Vec<FieldName>> {
        self.0.field_names()
    }

    fn get_posting_list_any_field(&self, term: &str) -> StorageBackendResult<PostingList> {
        self.0.get_posting_list_any_field(term)
    }

    fn doc_freq_any_field(&self, term: &str) -> StorageBackendResult<u64> {
        self.0.doc_freq_any_field(term)
    }

    fn get_total_doc_length(&self, doc_id: DocId) -> StorageBackendResult<u64> {
        self.0.get_total_doc_length(doc_id)
    }

    fn get_doc_lengths_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
    ) -> StorageBackendResult<BTreeMap<DocId, u64>> {
        self.0.get_doc_lengths_bulk(doc_ids, field)
    }

    fn get_term_freqs_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
        term: &str,
    ) -> StorageBackendResult<BTreeMap<DocId, u64>> {
        self.0.get_term_freqs_bulk(doc_ids, field, term)
    }

    fn get_scoring_inputs_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<(u64, Vec<u64>)>> {
        self.0.get_scoring_inputs_bulk(doc_ids, field, terms)
    }

    fn get_total_term_freq(&self, doc_id: DocId, term: &str) -> StorageBackendResult<u64> {
        self.0.get_total_term_freq(doc_id, term)
    }

    fn set_field_analyzer(
        &mut self,
        _field: &str,
        _analyzer: Analyzer,
        _phase: AnalyzerPhase,
    ) -> Result<(), String> {
        Err(read_only_error().to_string())
    }

    fn remove_field_analyzers(&mut self, _field: &str) -> Result<(), String> {
        Err(read_only_error().to_string())
    }

    fn get_field_analyzer(&self, field: &str) -> Analyzer {
        self.0.get_field_analyzer(field)
    }

    fn get_search_analyzer(&self, field: &str) -> Analyzer {
        self.0.get_search_analyzer(field)
    }

    fn index_analyzer_revision(
        &self,
        field: &str,
    ) -> StorageBackendResult<Arc<uqa_analysis::CompiledAnalyzer>> {
        self.0.index_analyzer_revision(field)
    }

    fn search_analyzer_revision(
        &self,
        field: &str,
    ) -> StorageBackendResult<Arc<uqa_analysis::CompiledAnalyzer>> {
        self.0.search_analyzer_revision(field)
    }

    fn set_field_analyzer_revision(
        &mut self,
        _field: &str,
        _revision: Arc<uqa_analysis::CompiledAnalyzer>,
        _phase: AnalyzerPhase,
    ) -> Result<(), String> {
        Err(read_only_error().to_string())
    }

    fn set_field_analyzer_revisions(
        &mut self,
        _field: &str,
        _index: Arc<uqa_analysis::CompiledAnalyzer>,
        _search: Arc<uqa_analysis::CompiledAnalyzer>,
    ) -> Result<(), String> {
        Err(read_only_error().to_string())
    }

    fn rebuild_with_analyzer_revision(
        &mut self,
        _field: &str,
        _revision: Arc<uqa_analysis::CompiledAnalyzer>,
        _phase: AnalyzerPhase,
        _documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn try_rebuild_documents_cancellable(
        &mut self,
        _documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
        _cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<()> {
        Err(read_only_error())
    }

    fn rebuild_with_analyzer_revision_cancellable(
        &mut self,
        _field: &str,
        _revision: Arc<uqa_analysis::CompiledAnalyzer>,
        _phase: AnalyzerPhase,
        _documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
        _cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<()> {
        Err(read_only_error())
    }
}
