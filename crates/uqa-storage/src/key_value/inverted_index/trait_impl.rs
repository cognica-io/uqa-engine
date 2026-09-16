//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Key/value index provider operations and retained revision installation.

use super::{
    Analyzer, AnalyzerPhase, Arc, BTreeMap, DocId, FieldName, IndexStats, IndexedFieldMetadata,
    InvertedIndex, KeyValueInvertedIndex, OccurrencePosting, PostingCursor, PostingList,
    StorageBackendResult, TokenTermKey,
};

impl InvertedIndex for KeyValueInvertedIndex {
    fn posting_read_cursor_key_budgeted<'a>(
        &'a self,
        field: &'a str,
        term: &'a TokenTermKey,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<crate::clustered_postings::BudgetedPostingReadCursor<'a>> {
        control.check()?;
        crate::clustered_postings::open_controlled_cursor(self.snapshot()?, field, term, control)
    }

    fn visit_score_clusters(
        &self,
        field: &str,
        term: &TokenTermKey,
        after: Option<u64>,
        limit: usize,
        control: &crate::read_control::StorageReadControl,
        visit: &mut crate::clustered_postings::ScoreClusterVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read(|view| view.visit_score_clusters(field, term, after, limit, control, visit))
    }

    fn get_occurrences_budgeted(
        &self,
        doc_id: DocId,
        field: &str,
        term: &TokenTermKey,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<uqa_core::memory::Budgeted<Vec<uqa_core::TokenOccurrence>>> {
        self.read(|view| view.get_occurrences_budgeted(doc_id, field, term, control))
    }

    fn field_stats_scalar_budgeted(
        &self,
        field: &str,
        control: &crate::read_control::StorageReadControl,
    ) -> StorageBackendResult<IndexStats> {
        self.read(|view| view.field_stats_scalar_budgeted(field, control))
    }

    fn analyzer(&self) -> &Analyzer {
        self.bindings.default_configuration()
    }

    fn source_rebuild_required(&self) -> StorageBackendResult<bool> {
        self.read(|view| view.source_rebuild_required())
    }

    fn add_document(
        &mut self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        self.add_documents(vec![(doc_id, fields)])
    }

    fn try_add_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        self.add_documents(documents)
    }

    fn remove_document(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.add_documents(vec![(doc_id, BTreeMap::new())])
    }

    fn try_rebuild_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        self.rebuild_documents(documents)
    }

    fn try_rebuild_documents_cancellable(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<()> {
        self.rebuild_documents_inner(documents, Some(cancellation))
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.mutate(|view, batch| view.clear_index_batch(batch))
    }

    fn get_posting_list(&self, field: &str, term: &str) -> StorageBackendResult<PostingList> {
        self.read(|view| view.get_posting_list(field, term))
    }

    fn get_posting_list_key(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<PostingList> {
        self.read(|view| view.get_posting_list_key(field, term))
    }

    fn posting_cursor(
        &self,
        field: &str,
        term: &str,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        self.read(|view| view.posting_cursor(field, term))
    }

    fn posting_cursor_key(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        self.read(|view| view.posting_cursor_key(field, term))
    }

    fn get_occurrence_postings(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Vec<OccurrencePosting>> {
        self.read(|view| view.get_occurrence_postings(field, term))
    }

    fn get_occurrences(
        &self,
        doc_id: DocId,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Vec<uqa_core::TokenOccurrence>> {
        self.read(|view| view.get_occurrences(doc_id, field, term))
    }

    fn indexed_field_metadata(
        &self,
        doc_id: DocId,
        field: &str,
    ) -> StorageBackendResult<Option<IndexedFieldMetadata>> {
        self.read(|view| view.indexed_field_metadata(doc_id, field))
    }

    fn for_each_term_freq(
        &self,
        field: &str,
        term: &str,
        visit: &mut dyn FnMut(DocId, u64),
    ) -> StorageBackendResult<()> {
        let mut cursor = self.posting_cursor(field, term)?;
        while let Some(entry) = cursor.current() {
            visit(entry.doc_id, entry.term_freq);
            cursor.advance()?;
        }
        Ok(())
    }

    fn doc_freq(&self, field: &str, term: &str) -> StorageBackendResult<u64> {
        self.read(|view| view.doc_freq(field, term))
    }

    fn doc_freq_key(&self, field: &str, term: &TokenTermKey) -> StorageBackendResult<u64> {
        self.read(|view| view.doc_freq_key(field, term))
    }

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> StorageBackendResult<u64> {
        self.read(|view| view.get_doc_length(doc_id, field))
    }

    fn get_scoring_inputs_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<(u64, Vec<u64>)>> {
        self.read(|view| view.get_scoring_inputs_bulk(doc_ids, field, terms))
    }

    fn get_scoring_inputs_keys_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
        terms: &[TokenTermKey],
    ) -> StorageBackendResult<Vec<(u64, Vec<u64>)>> {
        self.read(|view| view.get_scoring_inputs_keys_bulk(doc_ids, field, terms))
    }

    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> StorageBackendResult<u64> {
        self.read(|view| view.get_term_freq(doc_id, field, term))
    }

    fn get_term_freq_key(
        &self,
        doc_id: DocId,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<u64> {
        self.read(|view| view.get_term_freq_key(doc_id, field, term))
    }

    fn doc_count(&self) -> StorageBackendResult<u64> {
        self.read(|view| view.doc_count())
    }

    fn total_field_length(&self, field: &str) -> StorageBackendResult<u64> {
        self.read(|view| view.total_field_length(field))
    }

    fn field_doc_count(&self, field: &str) -> StorageBackendResult<u64> {
        self.read(|view| view.field_doc_count(field))
    }

    fn vocabulary_terms(&self, field: &str) -> StorageBackendResult<Vec<String>> {
        self.read(|view| view.vocabulary_terms(field))
    }

    fn vocabulary_keys(&self, field: &str) -> StorageBackendResult<Vec<TokenTermKey>> {
        self.read(|view| view.vocabulary_keys(field))
    }

    fn stats(&self) -> StorageBackendResult<IndexStats> {
        self.read(|view| view.stats())
    }

    fn posting_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        self.read(|view| view.posting_count(field))
    }

    fn doc_length_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        self.read(|view| view.doc_length_count(field))
    }

    fn term_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        self.read(|view| view.term_count(field))
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        self.retained_snapshot()
    }

    fn field_names(&self) -> StorageBackendResult<Vec<FieldName>> {
        self.read(|view| view.field_names())
    }

    fn set_field_analyzer(
        &mut self,
        field: &str,
        analyzer: Analyzer,
        phase: AnalyzerPhase,
    ) -> Result<(), String> {
        self.ensure_writable().map_err(|error| error.to_string())?;
        let mut candidate = self.bindings.clone();
        candidate
            .bind(field, &analyzer, phase)
            .map_err(|error| error.to_string())?;
        self.read(|view| view.validate_index_revision_change(field, &candidate))
            .map_err(|error| error.to_string())?;
        self.bindings = candidate;
        Ok(())
    }

    fn remove_field_analyzers(&mut self, field: &str) -> Result<(), String> {
        self.ensure_writable().map_err(|error| error.to_string())?;
        let mut candidate = self.bindings.clone();
        candidate.remove(field);
        self.read(|view| view.validate_index_revision_change(field, &candidate))
            .map_err(|error| error.to_string())?;
        self.bindings = candidate;
        Ok(())
    }

    fn get_field_analyzer(&self, field: &str) -> Analyzer {
        self.bindings.index_configuration(field).clone()
    }
    fn get_search_analyzer(&self, field: &str) -> Analyzer {
        self.bindings.search_configuration(field).clone()
    }
    fn index_analyzer_revision(
        &self,
        field: &str,
    ) -> StorageBackendResult<Arc<uqa_analysis::CompiledAnalyzer>> {
        Ok(self.bindings.index_revision(field)?)
    }
    fn search_analyzer_revision(
        &self,
        field: &str,
    ) -> StorageBackendResult<Arc<uqa_analysis::CompiledAnalyzer>> {
        Ok(self.bindings.search_revision(field)?)
    }

    fn set_field_analyzer_revision(
        &mut self,
        field: &str,
        revision: Arc<uqa_analysis::CompiledAnalyzer>,
        phase: AnalyzerPhase,
    ) -> Result<(), String> {
        self.ensure_writable().map_err(|error| error.to_string())?;
        let mut candidate = self.bindings.clone();
        candidate
            .bind_revision(field, revision, phase)
            .map_err(|error| error.to_string())?;
        self.read(|view| view.validate_index_revision_change(field, &candidate))
            .map_err(|error| error.to_string())?;
        self.bindings = candidate;
        Ok(())
    }

    fn set_field_analyzer_revisions(
        &mut self,
        field: &str,
        index: Arc<uqa_analysis::CompiledAnalyzer>,
        search: Arc<uqa_analysis::CompiledAnalyzer>,
    ) -> Result<(), String> {
        self.ensure_writable().map_err(|error| error.to_string())?;
        let mut candidate = self.bindings.clone();
        candidate
            .bind_revisions(field, index, search)
            .map_err(|error| error.to_string())?;
        self.read(|view| view.validate_index_revision_change(field, &candidate))
            .map_err(|error| error.to_string())?;
        self.bindings = candidate;
        Ok(())
    }

    fn rebuild_with_analyzer_revision(
        &mut self,
        field: &str,
        revision: Arc<uqa_analysis::CompiledAnalyzer>,
        phase: AnalyzerPhase,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        self.ensure_writable()?;
        let mut replacement = self.clone();
        replacement.bindings.bind_revision(field, revision, phase)?;
        replacement.rebuild_documents(documents)?;
        *self = replacement;
        Ok(())
    }

    fn rebuild_with_analyzer_revision_cancellable(
        &mut self,
        field: &str,
        revision: Arc<uqa_analysis::CompiledAnalyzer>,
        phase: AnalyzerPhase,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
        cancellation: &uqa_core::CancellationToken,
    ) -> StorageBackendResult<()> {
        cancellation.check()?;
        self.ensure_writable()?;
        let mut replacement = self.clone();
        replacement.bindings.bind_revision(field, revision, phase)?;
        replacement.rebuild_documents_inner(documents, Some(cancellation))?;
        *self = replacement;
        Ok(())
    }

    fn get_posting_lists_bulk(
        &self,
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<PostingList>> {
        self.read(|view| {
            terms
                .iter()
                .map(|term| view.get_posting_list(field, term))
                .collect()
        })
    }

    fn posting_cursors_bulk(
        &self,
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<Box<dyn PostingCursor>>> {
        self.read(|view| {
            terms
                .iter()
                .map(|term| view.posting_cursor(field, term))
                .collect()
        })
    }

    fn posting_cursors_keys_bulk(
        &self,
        field: &str,
        terms: &[TokenTermKey],
    ) -> StorageBackendResult<Vec<Box<dyn PostingCursor>>> {
        self.read(|view| {
            terms
                .iter()
                .map(|term| view.posting_cursor_key(field, term))
                .collect()
        })
    }

    fn get_posting_lists_keys_bulk(
        &self,
        field: &str,
        terms: &[TokenTermKey],
    ) -> StorageBackendResult<Vec<PostingList>> {
        self.read(|view| {
            terms
                .iter()
                .map(|term| view.get_posting_list_key(field, term))
                .collect()
        })
    }

    fn field_stats(&self, field: &str) -> StorageBackendResult<IndexStats> {
        self.read(|view| {
            let mut stats = view.stats()?;
            let field_docs = view.field_doc_count(field)?;
            stats.total_docs = field_docs;
            stats.avg_doc_length = if field_docs > 0 {
                view.total_field_length(field)? as f64 / field_docs as f64
            } else {
                0.0
            };
            Ok(stats)
        })
    }

    fn field_stats_scalar(&self, field: &str) -> StorageBackendResult<IndexStats> {
        self.read(|view| {
            let mut stats = IndexStats::default();
            let field_docs = view.field_doc_count(field)?;
            stats.total_docs = field_docs;
            stats.avg_doc_length = if field_docs > 0 {
                view.total_field_length(field)? as f64 / field_docs as f64
            } else {
                0.0
            };
            Ok(stats)
        })
    }

    fn get_posting_list_any_field(&self, term: &str) -> StorageBackendResult<PostingList> {
        self.read(|view| {
            let mut result = PostingList::new();
            for field in view.field_names()? {
                let pl = view.get_posting_list(&field, term)?;
                result = result.merge_union(&pl);
            }
            Ok(result)
        })
    }

    fn doc_freq_any_field(&self, term: &str) -> StorageBackendResult<u64> {
        self.read(|view| {
            let mut total = 0_u64;
            for field in view.field_names()? {
                total = total
                    .checked_add(view.doc_freq(&field, term)?)
                    .ok_or_else(|| super::other_error("document frequency overflow"))?;
            }
            Ok(total)
        })
    }

    fn get_total_doc_length(&self, doc_id: DocId) -> StorageBackendResult<u64> {
        self.read(|view| {
            let mut total = 0_u64;
            for field in view.field_names()? {
                total = total
                    .checked_add(view.get_doc_length(doc_id, &field)?)
                    .ok_or_else(|| super::other_error("document length overflow"))?;
            }
            Ok(total)
        })
    }

    fn get_total_term_freq(&self, doc_id: DocId, term: &str) -> StorageBackendResult<u64> {
        self.read(|view| {
            let mut total = 0_u64;
            for field in view.field_names()? {
                total = total
                    .checked_add(view.get_term_freq(doc_id, &field, term)?)
                    .ok_or_else(|| super::other_error("term frequency overflow"))?;
            }
            Ok(total)
        })
    }

    fn get_doc_lengths_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
    ) -> StorageBackendResult<BTreeMap<DocId, u64>> {
        self.read(|view| {
            let mut out = BTreeMap::new();
            for doc_id in doc_ids {
                out.insert(*doc_id, view.get_doc_length(*doc_id, field)?);
            }
            Ok(out)
        })
    }

    fn get_term_freqs_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
        term: &str,
    ) -> StorageBackendResult<BTreeMap<DocId, u64>> {
        self.read(|view| {
            let mut out = BTreeMap::new();
            for doc_id in doc_ids {
                out.insert(*doc_id, view.get_term_freq(*doc_id, field, term)?);
            }
            Ok(out)
        })
    }
}
