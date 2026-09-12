//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memory provider mutations, projections, and exact analyzer bindings.

use super::{
    checked_sum_u64, counter_error, usize_to_u64, Analyzer, AnalyzerBindings, AnalyzerPhase, Arc,
    BTreeMap, BTreeSet, DocId, FieldName, IndexStats, IndexedFieldMetadata, InvertedIndex,
    MaterializedPostingCursor, MemoryInvertedIndex, PostingCursor, PostingEntry, PostingList,
    PostingScore, StorageBackendError, StorageBackendResult, TokenOccurrence, TokenTermKey,
};

impl InvertedIndex for MemoryInvertedIndex {
    fn analyzer(&self) -> &Analyzer {
        self.bindings.default_configuration()
    }

    fn add_document(
        &mut self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        // Resolve and analyze every field before touching postings. A deferred default can fail even when another field already has a valid revision.
        let staged = self.stage_document(doc_id, fields)?;
        let plan = self.plan_replacement(doc_id, &staged.fields)?;
        self.apply_replacement(doc_id, staged, plan)
    }

    fn remove_document(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let Some(keys) = self.doc_terms.get(&doc_id).cloned() else {
            if self.doc_fields.contains_key(&doc_id) {
                return Err(StorageBackendError::Other(format!(
                    "inverted-index document {doc_id} has lengths but no reverse postings"
                )));
            }
            return Ok(());
        };
        let lengths = self.doc_fields.get(&doc_id).cloned().ok_or_else(|| {
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
        for (field, metadata) in &lengths {
            let total = self
                .total_length
                .get(field)
                .copied()
                .unwrap_or(0)
                .checked_sub(metadata.length)
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
        self.doc_fields.remove(&doc_id);
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
        let mut replacement = Self::with_bindings(self.bindings.clone());
        for (doc_id, fields) in documents {
            if !fields.is_empty() {
                replacement.add_document(doc_id, fields)?;
            }
        }
        *self = replacement;
        Ok(())
    }

    fn try_add_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        let mut replacement = self.clone();
        for (doc_id, fields) in documents {
            replacement.add_document(doc_id, fields)?;
        }
        *self = replacement;
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.index.clear();
        self.doc_terms.clear();
        self.doc_fields.clear();
        self.total_length.clear();
        self.field_doc_counts.clear();
        self.doc_count = 0;
        Ok(())
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
            .index
            .get(&(field.to_owned(), term.clone()))
            .into_iter()
            .flat_map(|postings| postings.values())
            .map(|posting| posting.projection.clone())
            .collect();
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn posting_cursor(
        &self,
        field: &str,
        term: &str,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        self.posting_cursor_key(field, &TokenTermKey::from_text(term))
    }

    fn posting_cursor_key(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        let entries = self
            .index
            .get(&(field.to_owned(), term.clone()))
            .into_iter()
            .flat_map(|postings| postings.values())
            .map(|posting| {
                Ok(PostingScore {
                    doc_id: posting.projection.doc_id,
                    term_freq: usize_to_u64(posting.occurrences.len(), "term frequency")?,
                    doc_length: self.get_doc_length(posting.projection.doc_id, field)?,
                })
            })
            .collect::<StorageBackendResult<Vec<_>>>()?;
        Ok(Box::new(MaterializedPostingCursor::new(entries)?))
    }

    fn get_occurrence_postings(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Vec<crate::clustered_postings::OccurrencePosting>> {
        self.index
            .get(&(field.to_owned(), term.clone()))
            .into_iter()
            .flat_map(|postings| postings.values())
            .map(|posting| {
                Ok(crate::clustered_postings::OccurrencePosting {
                    doc_id: posting.projection.doc_id,
                    doc_length: self.get_doc_length(posting.projection.doc_id, field)?,
                    occurrences: posting.occurrences.clone(),
                })
            })
            .collect()
    }

    fn get_occurrences(
        &self,
        doc_id: DocId,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Vec<TokenOccurrence>> {
        Ok(self
            .index
            .get(&(field.to_owned(), term.clone()))
            .and_then(|postings| postings.get(&doc_id))
            .map_or_else(Vec::new, |posting| posting.occurrences.clone()))
    }

    fn indexed_field_metadata(
        &self,
        doc_id: DocId,
        field: &str,
    ) -> StorageBackendResult<Option<IndexedFieldMetadata>> {
        Ok(self
            .doc_fields
            .get(&doc_id)
            .and_then(|fields| fields.get(field))
            .copied())
    }

    fn for_each_posting(
        &self,
        field: &str,
        term: &str,
        visit: &mut dyn FnMut(&PostingEntry),
    ) -> StorageBackendResult<()> {
        if let Some(postings) = self
            .index
            .get(&(field.to_owned(), TokenTermKey::from_text(term)))
        {
            for posting in postings.values() {
                visit(&posting.projection);
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
        if let Some(postings) = self
            .index
            .get(&(field.to_owned(), TokenTermKey::from_text(term)))
        {
            for posting in postings.values() {
                visit(
                    posting.projection.doc_id,
                    usize_to_u64(posting.occurrences.len(), "term frequency")?,
                );
            }
        }
        Ok(())
    }

    fn doc_freq(&self, field: &str, term: &str) -> StorageBackendResult<u64> {
        self.doc_freq_key(field, &TokenTermKey::from_text(term))
    }

    fn doc_freq_key(&self, field: &str, term: &TokenTermKey) -> StorageBackendResult<u64> {
        self.index
            .get(&(field.to_owned(), term.clone()))
            .map_or(Ok(0), |postings| {
                usize_to_u64(postings.len(), "document frequency")
            })
    }

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> StorageBackendResult<u64> {
        Ok(self
            .doc_fields
            .get(&doc_id)
            .and_then(|fields| fields.get(field))
            .map_or(0, |metadata| metadata.length))
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
        self.index
            .get(&(field.to_owned(), term.clone()))
            .and_then(|postings| postings.get(&doc_id))
            .map_or(Ok(0), |posting| {
                usize_to_u64(posting.occurrences.len(), "term frequency")
            })
    }

    fn doc_count(&self) -> StorageBackendResult<u64> {
        Ok(self.doc_count)
    }

    fn total_field_length(&self, field: &str) -> StorageBackendResult<u64> {
        Ok(self.total_length.get(field).copied().unwrap_or(0))
    }

    fn vocabulary_terms(&self, field: &str) -> StorageBackendResult<Vec<String>> {
        self.vocabulary_keys(field)?
            .into_iter()
            .map(|key| Ok(key.to_term().into_string()?))
            .collect()
    }

    fn vocabulary_keys(&self, field: &str) -> StorageBackendResult<Vec<TokenTermKey>> {
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
            let term = term.to_term();
            let frequency = usize_to_u64(inner.len(), "document frequency")?;
            if let Some(text) = term.as_str() {
                s.set_doc_freq(field.clone(), text, frequency);
            } else {
                s.set_doc_freq_utf16(field.clone(), term.into_utf16(), frequency);
            }
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
        let mut candidate = self.bindings.clone();
        candidate
            .bind(field, &analyzer, phase)
            .map_err(|error| error.to_string())?;
        self.validate_index_revision_change(field, &candidate)?;
        self.bindings = candidate;
        Ok(())
    }

    fn remove_field_analyzers(&mut self, field: &str) -> Result<(), String> {
        let mut candidate = self.bindings.clone();
        candidate.remove(field);
        self.validate_index_revision_change(field, &candidate)?;
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
        self.validate_index_revision_change(field, &candidate)?;
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
        self.validate_index_revision_change(field, &candidate)?;
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
        let mut replacement = Self::with_bindings(self.bindings.clone());
        replacement
            .set_field_analyzer_revision(field, revision, phase)
            .map_err(crate::StorageBackendError::Other)?;
        replacement.try_rebuild_documents(documents)?;
        *self = replacement;
        Ok(())
    }
}

impl MemoryInvertedIndex {
    fn validate_index_revision_change(
        &self,
        field: &str,
        candidate: &AnalyzerBindings,
    ) -> Result<(), String> {
        if self.field_doc_counts.get(field).copied().unwrap_or(0) > 0 {
            let current = self
                .bindings
                .index_revision(field)
                .map_err(|error| error.to_string())?;
            let proposed = candidate
                .index_revision(field)
                .map_err(|error| error.to_string())?;
            if current.descriptor().fingerprint() != proposed.descriptor().fingerprint() {
                return Err(format!("field `{field}` has indexed documents; changing its index analyzer requires an atomic source rebuild"));
            }
        }
        Ok(())
    }
}
