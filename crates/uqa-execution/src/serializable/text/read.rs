//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Demand-driven observations preserve native posting cursors and bulk reads.

use std::{collections::BTreeMap, ops::Deref, sync::Arc};
use uqa_analysis::{Analyzer, CompiledAnalyzer};
use uqa_core::{
    memory::Budgeted, DocId, FieldName, IndexStats, PostingEntry, PostingList, TokenOccurrence,
};
use uqa_sql::ast::ColumnDef;
use uqa_storage::{
    clustered_postings::{
        BudgetedPostingReadCursor, OccurrencePosting, PostingCursor, PostingReadCursor,
        ScoreClusterVisitor,
    },
    inverted_index::IndexedFieldMetadata,
    read_control::StorageReadControl,
    BlockMaxScorer, InvertedIndex, StorageBackendError, StorageBackendResult, TokenTermKey,
};

use super::{TextObservation, DOCUMENT, POSTING, STATISTICS};
use crate::serializable::SerializableRelationRead;

/// A borrowed live index or owned retained snapshot with its original participant. Construction and analyzer metadata access do not observe a query.
pub struct ObservedTextIndex<I> {
    index: I,
    observation: Option<TextObservation>,
}

impl<I> ObservedTextIndex<I> {
    pub fn new(
        index: I,
        read: Option<SerializableRelationRead>,
        columns: Arc<Vec<ColumnDef>>,
    ) -> Self {
        Self {
            index,
            observation: read.map(|read| TextObservation { read, columns }),
        }
    }

    fn observe(
        &self,
        operation: impl FnOnce(&TextObservation) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.observation.as_ref().map_or(Ok(()), operation)
    }
}

impl<I: Deref<Target = dyn InvertedIndex>> ObservedTextIndex<I> {
    fn observe_fields(
        &self,
        operation: impl Fn(&TextObservation, &str) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.observe(|observation| {
            observation.field_range(STATISTICS, None)?;
            for field in self.index.field_names()? {
                operation(observation, &field)?;
            }
            Ok(())
        })
    }
}

pub fn observe_snapshot(
    read: Option<&SerializableRelationRead>,
    columns: Arc<Vec<ColumnDef>>,
    index: Arc<dyn InvertedIndex>,
) -> Arc<dyn InvertedIndex> {
    match read {
        Some(read) => Arc::new(ObservedTextIndex::new(index, Some(read.clone()), columns)),
        None => index,
    }
}

fn read_only() -> StorageBackendError {
    StorageBackendError::Other("cannot write a retained serializable text reader".into())
}

#[cfg(test)]
mod tests;

impl<I: Deref<Target = dyn InvertedIndex> + Send + Sync> InvertedIndex for ObservedTextIndex<I> {
    fn analyzer(&self) -> &Analyzer {
        self.index.analyzer()
    }
    fn source_rebuild_required(&self) -> StorageBackendResult<bool> {
        self.index.source_rebuild_required()
    }
    fn get_field_analyzer(&self, field: &str) -> Analyzer {
        self.index.get_field_analyzer(field)
    }
    fn get_search_analyzer(&self, field: &str) -> Analyzer {
        self.index.get_search_analyzer(field)
    }
    fn index_analyzer_revision(&self, field: &str) -> StorageBackendResult<Arc<CompiledAnalyzer>> {
        self.index.index_analyzer_revision(field)
    }
    fn search_analyzer_revision(&self, field: &str) -> StorageBackendResult<Arc<CompiledAnalyzer>> {
        self.index.search_analyzer_revision(field)
    }
    fn add_document(
        &mut self,
        _: DocId,
        _: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn try_add_documents(
        &mut self,
        _: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn remove_document(&mut self, _: DocId) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn rebuild_persisted_block_max(
        &mut self,
        _: &str,
        _: &dyn BlockMaxScorer,
        _: &str,
    ) -> StorageBackendResult<bool> {
        Err(read_only())
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        Ok(Arc::new(ObservedTextIndex {
            index: self.index.snapshot()?,
            observation: self.observation.clone(),
        }))
    }

    fn snapshot_with_control(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        Ok(Arc::new(ObservedTextIndex {
            index: self.index.snapshot_with_control(control)?,
            observation: self.observation.clone(),
        }))
    }

    fn get_posting_list(&self, field: &str, term: &str) -> StorageBackendResult<PostingList> {
        self.observe(|o| o.scalar_term(field, term, None))?;
        self.index.get_posting_list(field, term)
    }
    fn get_posting_list_key(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<PostingList> {
        self.observe(|o| o.term(field, term, None))?;
        self.index.get_posting_list_key(field, term)
    }
    fn get_posting_lists_bulk(
        &self,
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<PostingList>> {
        self.observe(|o| {
            terms
                .iter()
                .try_for_each(|term| o.scalar_term(field, term, None))
        })?;
        self.index.get_posting_lists_bulk(field, terms)
    }
    fn get_posting_lists_keys_bulk(
        &self,
        field: &str,
        terms: &[TokenTermKey],
    ) -> StorageBackendResult<Vec<PostingList>> {
        self.observe(|o| terms.iter().try_for_each(|term| o.term(field, term, None)))?;
        self.index.get_posting_lists_keys_bulk(field, terms)
    }
    fn posting_cursor(
        &self,
        field: &str,
        term: &str,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        self.observe(|o| o.scalar_term(field, term, None))?;
        self.index.posting_cursor(field, term)
    }
    fn posting_cursor_key(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        self.observe(|o| o.term(field, term, None))?;
        self.index.posting_cursor_key(field, term)
    }
    fn posting_cursors_bulk(
        &self,
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<Box<dyn PostingCursor>>> {
        self.observe(|o| {
            terms
                .iter()
                .try_for_each(|term| o.scalar_term(field, term, None))
        })?;
        self.index.posting_cursors_bulk(field, terms)
    }
    fn posting_cursors_keys_bulk(
        &self,
        field: &str,
        terms: &[TokenTermKey],
    ) -> StorageBackendResult<Vec<Box<dyn PostingCursor>>> {
        self.observe(|o| terms.iter().try_for_each(|term| o.term(field, term, None)))?;
        self.index.posting_cursors_keys_bulk(field, terms)
    }
    fn posting_read_cursor_key<'a>(
        &'a self,
        field: &'a str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Box<dyn PostingReadCursor + 'a>> {
        self.observe(|o| o.term(field, term, None))?;
        self.index.posting_read_cursor_key(field, term)
    }
    fn posting_read_cursor_key_budgeted<'a>(
        &'a self,
        field: &'a str,
        term: &'a TokenTermKey,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedPostingReadCursor<'a>> {
        self.observe(|o| o.term(field, term, None))?;
        self.index
            .posting_read_cursor_key_budgeted(field, term, control)
    }
    fn visit_score_clusters(
        &self,
        field: &str,
        term: &TokenTermKey,
        after: Option<u64>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut ScoreClusterVisitor<'_>,
    ) -> StorageBackendResult<()> {
        if limit != 0 {
            self.observe(|o| o.term(field, term, None))?;
        }
        self.index
            .visit_score_clusters(field, term, after, limit, control, visit)
    }
    fn get_occurrence_postings(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Vec<OccurrencePosting>> {
        self.observe(|o| o.term(field, term, None))?;
        self.index.get_occurrence_postings(field, term)
    }
    fn get_occurrences(
        &self,
        doc: DocId,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Vec<TokenOccurrence>> {
        self.observe(|o| o.term(field, term, Some(doc)))?;
        self.index.get_occurrences(doc, field, term)
    }
    fn get_occurrences_budgeted(
        &self,
        doc: DocId,
        field: &str,
        term: &TokenTermKey,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Vec<TokenOccurrence>>> {
        self.observe(|o| o.term(field, term, Some(doc)))?;
        self.index
            .get_occurrences_budgeted(doc, field, term, control)
    }
    fn indexed_field_metadata(
        &self,
        doc: DocId,
        field: &str,
    ) -> StorageBackendResult<Option<IndexedFieldMetadata>> {
        self.observe(|o| o.document(field, doc))?;
        self.index.indexed_field_metadata(doc, field)
    }
    fn doc_freq(&self, field: &str, term: &str) -> StorageBackendResult<u64> {
        self.observe(|o| o.scalar_term(field, term, None))?;
        self.index.doc_freq(field, term)
    }
    fn doc_freq_key(&self, field: &str, term: &TokenTermKey) -> StorageBackendResult<u64> {
        self.observe(|o| o.term(field, term, None))?;
        self.index.doc_freq_key(field, term)
    }
    fn get_term_freq(&self, doc: DocId, field: &str, term: &str) -> StorageBackendResult<u64> {
        self.observe(|o| o.scalar_term(field, term, Some(doc)))?;
        self.index.get_term_freq(doc, field, term)
    }
    fn get_term_freq_key(
        &self,
        doc: DocId,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<u64> {
        self.observe(|o| o.term(field, term, Some(doc)))?;
        self.index.get_term_freq_key(doc, field, term)
    }
    fn get_doc_length(&self, doc: DocId, field: &str) -> StorageBackendResult<u64> {
        self.observe(|o| o.document(field, doc))?;
        self.index.get_doc_length(doc, field)
    }
    fn doc_count(&self) -> StorageBackendResult<u64> {
        self.observe(|o| o.field_range(DOCUMENT, None))?;
        self.index.doc_count()
    }
    fn total_field_length(&self, field: &str) -> StorageBackendResult<u64> {
        self.observe(|o| o.statistics(field))?;
        self.index.total_field_length(field)
    }
    fn field_doc_count(&self, field: &str) -> StorageBackendResult<u64> {
        self.observe(|o| o.statistics(field))?;
        self.index.field_doc_count(field)
    }
    fn field_stats_scalar(&self, field: &str) -> StorageBackendResult<IndexStats> {
        self.observe(|o| o.statistics(field))?;
        self.index.field_stats_scalar(field)
    }
    fn field_stats_scalar_budgeted(
        &self,
        field: &str,
        control: &StorageReadControl,
    ) -> StorageBackendResult<IndexStats> {
        self.observe(|o| o.statistics(field))?;
        self.index.field_stats_scalar_budgeted(field, control)
    }
    fn field_stats(&self, field: &str) -> StorageBackendResult<IndexStats> {
        self.observe(TextObservation::all)?;
        self.index.field_stats(field)
    }
    fn stats(&self) -> StorageBackendResult<IndexStats> {
        self.observe(TextObservation::all)?;
        self.index.stats()
    }
    fn vocabulary_terms(&self, field: &str) -> StorageBackendResult<Vec<String>> {
        self.observe(|o| o.field_range(POSTING, Some(field)))?;
        self.index.vocabulary_terms(field)
    }
    fn vocabulary_keys(&self, field: &str) -> StorageBackendResult<Vec<TokenTermKey>> {
        self.observe(|o| o.field_range(POSTING, Some(field)))?;
        self.index.vocabulary_keys(field)
    }
    fn posting_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        self.observe(|o| o.field_range(POSTING, field))?;
        self.index.posting_count(field)
    }
    fn doc_length_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        self.observe(|o| o.field_range(DOCUMENT, field))?;
        self.index.doc_length_count(field)
    }
    fn term_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        self.observe(|o| o.field_range(POSTING, field))?;
        self.index.term_count(field)
    }
    fn field_names(&self) -> StorageBackendResult<Vec<FieldName>> {
        self.observe(|o| o.field_range(STATISTICS, None))?;
        self.index.field_names()
    }
    fn get_posting_list_any_field(&self, term: &str) -> StorageBackendResult<PostingList> {
        self.observe_fields(|o, field| o.scalar_term(field, term, None))?;
        self.index.get_posting_list_any_field(term)
    }
    fn doc_freq_any_field(&self, term: &str) -> StorageBackendResult<u64> {
        self.observe_fields(|o, field| o.scalar_term(field, term, None))?;
        self.index.doc_freq_any_field(term)
    }
    fn get_total_doc_length(&self, doc: DocId) -> StorageBackendResult<u64> {
        self.observe_fields(|o, field| o.document(field, doc))?;
        self.index.get_total_doc_length(doc)
    }
    fn get_total_term_freq(&self, doc: DocId, term: &str) -> StorageBackendResult<u64> {
        self.observe_fields(|o, field| o.scalar_term(field, term, Some(doc)))?;
        self.index.get_total_term_freq(doc, term)
    }
    fn for_each_posting(
        &self,
        field: &str,
        term: &str,
        visit: &mut dyn FnMut(&PostingEntry),
    ) -> StorageBackendResult<()> {
        self.observe(|o| o.scalar_term(field, term, None))?;
        self.index.for_each_posting(field, term, visit)
    }
    fn for_each_term_freq(
        &self,
        field: &str,
        term: &str,
        visit: &mut dyn FnMut(DocId, u64),
    ) -> StorageBackendResult<()> {
        self.observe(|o| o.scalar_term(field, term, None))?;
        self.index.for_each_term_freq(field, term, visit)
    }
    fn persisted_block_max_scores(
        &self,
        field: &str,
        term: &str,
        fingerprint: &str,
    ) -> StorageBackendResult<Option<Vec<f64>>> {
        self.observe(|o| o.scalar_term(field, term, None))?;
        self.index
            .persisted_block_max_scores(field, term, fingerprint)
    }
    fn persisted_block_max_scores_bulk(
        &self,
        field: &str,
        terms: &[String],
        fingerprint: &str,
    ) -> StorageBackendResult<Vec<Option<Vec<f64>>>> {
        self.observe(|o| {
            terms
                .iter()
                .try_for_each(|term| o.scalar_term(field, term, None))
        })?;
        self.index
            .persisted_block_max_scores_bulk(field, terms, fingerprint)
    }
    fn persisted_block_max_scores_keys_bulk(
        &self,
        field: &str,
        terms: &[TokenTermKey],
        fingerprint: &str,
    ) -> StorageBackendResult<Vec<Option<Vec<f64>>>> {
        self.observe(|o| terms.iter().try_for_each(|term| o.term(field, term, None)))?;
        self.index
            .persisted_block_max_scores_keys_bulk(field, terms, fingerprint)
    }
    fn get_doc_lengths_bulk(
        &self,
        docs: &[DocId],
        field: &str,
    ) -> StorageBackendResult<BTreeMap<DocId, u64>> {
        self.observe(|o| docs.iter().try_for_each(|doc| o.document(field, *doc)))?;
        self.index.get_doc_lengths_bulk(docs, field)
    }
    fn get_term_freqs_bulk(
        &self,
        docs: &[DocId],
        field: &str,
        term: &str,
    ) -> StorageBackendResult<BTreeMap<DocId, u64>> {
        self.observe(|o| {
            docs.iter()
                .try_for_each(|doc| o.scalar_term(field, term, Some(*doc)))
        })?;
        self.index.get_term_freqs_bulk(docs, field, term)
    }
    fn get_scoring_inputs_bulk(
        &self,
        docs: &[DocId],
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<(u64, Vec<u64>)>> {
        self.observe(|o| {
            docs.iter().try_for_each(|doc| {
                o.document(field, *doc)?;
                terms
                    .iter()
                    .try_for_each(|term| o.scalar_term(field, term, Some(*doc)))
            })
        })?;
        self.index.get_scoring_inputs_bulk(docs, field, terms)
    }
    fn get_scoring_inputs_keys_bulk(
        &self,
        docs: &[DocId],
        field: &str,
        terms: &[TokenTermKey],
    ) -> StorageBackendResult<Vec<(u64, Vec<u64>)>> {
        self.observe(|o| {
            docs.iter().try_for_each(|doc| {
                o.document(field, *doc)?;
                terms
                    .iter()
                    .try_for_each(|term| o.term(field, term, Some(*doc)))
            })
        })?;
        self.index.get_scoring_inputs_keys_bulk(docs, field, terms)
    }
}
