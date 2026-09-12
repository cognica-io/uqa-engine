//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Key/value index provider operations and retained revision installation.

use super::super::codec::usize_to_u64;
use super::queries::require_score_version;
use super::{
    cluster_id, decode_all_scores, keys, other_error, score_count, Analyzer, AnalyzerPhase, Arc,
    BTreeMap, BTreeSet, DocId, FieldName, IndexStats, IndexedFieldMetadata, InvertedIndex,
    KeyValueInvertedIndex, OccurrencePosting, Payload, PostingCursor, PostingEntry, PostingList,
    StorageBackendResult, TokenTermKey,
};

impl InvertedIndex for KeyValueInvertedIndex {
    fn analyzer(&self) -> &Analyzer {
        self.bindings.default_configuration()
    }

    fn source_rebuild_required(&self) -> StorageBackendResult<bool> {
        self.needs_source_rebuild()
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

    fn clear(&mut self) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        self.clear_index_batch(batch.as_mut())?;
        batch.commit()
    }

    fn get_posting_list(&self, field: &str, term: &str) -> StorageBackendResult<PostingList> {
        self.get_posting_list_key(field, &TokenTermKey::from_text(term))
    }

    fn get_posting_list_key(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<PostingList> {
        let entries = self
            .occurrence_postings(field, term)?
            .into_iter()
            .map(|entry| {
                PostingEntry::new(
                    entry.doc_id,
                    Payload {
                        positions: entry.positions(),
                        score: 0.0,
                        fields: BTreeMap::new(),
                    },
                )
            })
            .collect();
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn posting_cursor(
        &self,
        field: &str,
        term: &str,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        self.cursor_for_term(field, &TokenTermKey::from_text(term))
    }

    fn posting_cursor_key(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        self.cursor_for_term(field, term)
    }

    fn get_occurrence_postings(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Vec<OccurrencePosting>> {
        self.occurrence_postings(field, term)
    }

    fn get_occurrences(
        &self,
        doc_id: DocId,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Vec<uqa_core::TokenOccurrence>> {
        self.require_graph_format()?;
        let entries = self.load_cluster(field, term, cluster_id(doc_id))?;
        let Some(posting) = entries.into_iter().find(|entry| entry.doc_id == doc_id) else {
            return Ok(Vec::new());
        };
        self.validate_posting_metadata(field, &posting)?;
        Ok(posting.occurrences)
    }

    fn indexed_field_metadata(
        &self,
        doc_id: DocId,
        field: &str,
    ) -> StorageBackendResult<Option<IndexedFieldMetadata>> {
        self.require_graph_format()?;
        self.read_field_metadata(doc_id, field)
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
        self.doc_freq_key(field, &TokenTermKey::from_text(term))
    }

    fn doc_freq_key(&self, field: &str, term: &TokenTermKey) -> StorageBackendResult<u64> {
        self.require_graph_format()?;
        self.store
            .scan_prefix(&keys::term_prefix(&self.table, keys::SCORE, field, term)?)?
            .into_iter()
            .try_fold(0_u64, |total, (key, score)| {
                keys::read_cluster(&key, keys::SCORE)?;
                require_score_version(&score)?;
                total
                    .checked_add(score_count(&score)?)
                    .ok_or_else(|| other_error("document frequency overflow"))
            })
    }

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> StorageBackendResult<u64> {
        self.document_length(doc_id, field)
    }

    fn get_scoring_inputs_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<(u64, Vec<u64>)>> {
        let keys = terms
            .iter()
            .map(|term| TokenTermKey::from_text(term))
            .collect::<Vec<_>>();
        self.get_scoring_inputs_keys_bulk(doc_ids, field, &keys)
    }

    fn get_scoring_inputs_keys_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
        terms: &[TokenTermKey],
    ) -> StorageBackendResult<Vec<(u64, Vec<u64>)>> {
        let mut output = doc_ids
            .iter()
            .map(|id| Ok((self.document_length(*id, field)?, vec![0; terms.len()])))
            .collect::<StorageBackendResult<Vec<_>>>()?;
        let mut positions = BTreeMap::<DocId, Vec<usize>>::new();
        for (position, doc_id) in doc_ids.iter().copied().enumerate() {
            positions.entry(doc_id).or_default().push(position);
        }
        for (term_index, term) in terms.iter().enumerate() {
            let mut cursor = self.posting_cursor_key(field, term)?;
            while let Some(entry) = cursor.current() {
                if let Some(output_positions) = positions.get(&entry.doc_id) {
                    for position in output_positions {
                        output[*position].0 = entry.doc_length;
                        output[*position].1[term_index] = entry.term_freq;
                    }
                }
                cursor.advance()?;
            }
        }
        Ok(output)
    }

    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> StorageBackendResult<u64> {
        self.get_term_freq_key(doc_id, field, &TokenTermKey::from_text(term))
    }

    fn get_term_freq_key(
        &self,
        doc_id: DocId,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<u64> {
        self.require_graph_format()?;
        let cluster = cluster_id(doc_id);
        self.store
            .get(&keys::cluster_key(
                &self.table,
                keys::SCORE,
                field,
                term,
                cluster,
            )?)?
            .map_or(Ok(0), |score| {
                require_score_version(&score)?;
                let entries = decode_all_scores(cluster, &score)?;
                Ok(entries
                    .binary_search_by_key(&doc_id, |entry| entry.doc_id)
                    .ok()
                    .map_or(0, |position| entries[position].term_freq))
            })
    }

    fn doc_count(&self) -> StorageBackendResult<u64> {
        self.require_graph_format()?;
        let mut doc_ids = BTreeSet::new();
        for (key, _) in self
            .store
            .scan_prefix(&keys::kind_prefix(&self.table, keys::LENGTH)?)?
        {
            doc_ids.insert(keys::read_document(&key, keys::LENGTH)?.0);
        }
        usize_to_u64(doc_ids.len(), "document count")
    }

    fn total_field_length(&self, field: &str) -> StorageBackendResult<u64> {
        self.require_graph_format()?;
        Ok(self
            .stored_field_stats(field)?
            .map_or(0, |stats| stats.total_length))
    }

    fn field_doc_count(&self, field: &str) -> StorageBackendResult<u64> {
        self.require_graph_format()?;
        Ok(self
            .stored_field_stats(field)?
            .map_or(0, |stats| stats.doc_count))
    }

    fn vocabulary_terms(&self, field: &str) -> StorageBackendResult<Vec<String>> {
        self.vocabulary_keys(field)?
            .into_iter()
            .map(|key| Ok(key.to_term().into_string()?))
            .collect()
    }

    fn vocabulary_keys(&self, field: &str) -> StorageBackendResult<Vec<TokenTermKey>> {
        self.indexed_terms(Some(field))
    }

    fn stats(&self) -> StorageBackendResult<IndexStats> {
        self.index_statistics()
    }

    fn posting_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        self.store
            .scan_prefix(&self.score_prefix(field)?)?
            .into_iter()
            .try_fold(0_u64, |total, (key, value)| {
                keys::read_cluster(&key, keys::SCORE)?;
                require_score_version(&value)?;
                total
                    .checked_add(score_count(&value)?)
                    .ok_or_else(|| other_error("posting count overflow"))
            })
    }

    fn doc_length_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        self.require_graph_format()?;
        if let Some(field) = field {
            return self.field_doc_count(field);
        }
        let mut count = 0_u64;
        for (key, _) in self
            .store
            .scan_prefix(&keys::kind_prefix(&self.table, keys::LENGTH)?)?
        {
            keys::read_document(&key, keys::LENGTH)?;
            count = count
                .checked_add(1)
                .ok_or_else(|| other_error("document-length row count overflow"))?;
        }
        Ok(count)
    }

    fn term_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        usize_to_u64(self.indexed_terms(field)?.len(), "term count")
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        Ok(Arc::new(self.clone()))
    }

    fn field_names(&self) -> StorageBackendResult<Vec<FieldName>> {
        self.require_graph_format()?;
        let mut fields = Vec::new();
        for (key, value) in self
            .store
            .scan_prefix(&keys::kind_prefix(&self.table, keys::FIELD)?)?
        {
            super::FieldStats::from_bytes(&value)?;
            fields.push(keys::read_field(&key)?);
        }
        fields.sort();
        Ok(fields)
    }

    fn set_field_analyzer(
        &mut self,
        field: &str,
        analyzer: Analyzer,
        phase: AnalyzerPhase,
    ) -> Result<(), String> {
        let mut candidate = self.bindings.clone();
        candidate
            .bind(field, &analyzer, phase)
            .map_err(|error| error.to_string())?;
        self.validate_index_revision_change(field, &candidate)
            .map_err(|error| error.to_string())?;
        self.bindings = candidate;
        Ok(())
    }

    fn remove_field_analyzers(&mut self, field: &str) -> Result<(), String> {
        let mut candidate = self.bindings.clone();
        candidate.remove(field);
        self.validate_index_revision_change(field, &candidate)
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
        let mut candidate = self.bindings.clone();
        candidate
            .bind_revision(field, revision, phase)
            .map_err(|error| error.to_string())?;
        self.validate_index_revision_change(field, &candidate)
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
        let mut candidate = self.bindings.clone();
        candidate
            .bind_revisions(field, index, search)
            .map_err(|error| error.to_string())?;
        self.validate_index_revision_change(field, &candidate)
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
        let mut replacement = self.clone();
        replacement.bindings.bind_revision(field, revision, phase)?;
        replacement.rebuild_documents(documents)?;
        *self = replacement;
        Ok(())
    }
}
