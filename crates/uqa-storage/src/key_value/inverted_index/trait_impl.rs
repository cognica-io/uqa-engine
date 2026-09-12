//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Key/value index provider operations and retained revision installation.

use super::{
    cluster_id, decode_all_scores, decode_cluster, decode_u64_value, doc_length_key,
    doc_length_key_prefix, encode_cluster, encode_terms, field_stats_key, field_stats_key_prefix,
    other_error, posting_cluster_positions_key, posting_cluster_positions_key_prefix,
    posting_cluster_score_field_prefix, posting_cluster_score_key,
    posting_cluster_score_key_prefix, posting_cluster_score_term_prefix,
    posting_document_doc_prefix, posting_document_key, posting_document_key_prefix, read_str,
    read_u64, score_count, u64_value, usize_to_u64, Analyzer, AnalyzerPhase, Arc, BTreeMap,
    BTreeSet, ClusterKey, ClusterPosting, DocId, FieldName, IndexStats, InvertedIndex,
    KeyValueInvertedIndex, Payload, PostingCursor, PostingEntry, PostingList, StorageBackendResult,
};

impl InvertedIndex for KeyValueInvertedIndex {
    fn analyzer(&self) -> &Analyzer {
        self.bindings.default_configuration()
    }

    fn add_document(
        &mut self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        let old_lengths = self.old_doc_lengths(doc_id)?;
        let old_terms = self.old_terms(doc_id)?;
        let (new_lengths, new_postings) = self.analyze_fields(fields)?;
        let cluster_changes =
            self.stage_cluster_changes(doc_id, &old_terms, &new_lengths, &new_postings)?;

        let mut fields_to_update = BTreeSet::new();
        fields_to_update.extend(old_lengths.keys().cloned());
        fields_to_update.extend(new_lengths.keys().cloned());
        let mut totals = Vec::with_capacity(fields_to_update.len());
        for field in fields_to_update {
            let base = self
                .store
                .get(&field_stats_key(&self.table, &field)?)?
                .map(|value| decode_u64_value(&value))
                .transpose()?
                .unwrap_or(0);
            let old = old_lengths.get(&field).copied().unwrap_or(0);
            let new = new_lengths.get(&field).copied().unwrap_or(0);
            let total = base
                .checked_sub(old)
                .ok_or_else(|| other_error("stored field length is smaller than document length"))?
                .checked_add(new)
                .ok_or_else(|| other_error("total field length overflow"))?;
            totals.push((field, total));
        }

        let mut terms_by_field = BTreeMap::<FieldName, Vec<String>>::new();
        for (field, term, _) in &new_postings {
            terms_by_field
                .entry(field.clone())
                .or_default()
                .push(term.clone());
        }
        for field in new_lengths.keys() {
            terms_by_field.entry(field.clone()).or_default();
        }

        let mut batch = self.store.batch();
        Self::apply_cluster_changes(batch.as_mut(), &self.table, cluster_changes)?;
        batch.delete_prefix(&posting_document_doc_prefix(&self.table, doc_id)?)?;
        for field in old_lengths.keys() {
            batch.delete(&doc_length_key(&self.table, doc_id, field)?)?;
        }
        for (field, total) in totals {
            Self::set_total_length(batch.as_mut(), &self.table, &field, total)?;
        }
        for (field, length) in &new_lengths {
            batch.put(
                &doc_length_key(&self.table, doc_id, field)?,
                &u64_value(*length),
            )?;
        }
        for (field, terms) in terms_by_field {
            batch.put(
                &posting_document_key(&self.table, doc_id, &field)?,
                &encode_terms(&terms)?,
            )?;
        }
        batch.commit()
    }

    fn try_add_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        self.add_documents(documents)
    }

    fn remove_document(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let old_lengths = self.old_doc_lengths(doc_id)?;
        let old_terms = self.old_terms(doc_id)?;
        let cluster_changes =
            self.stage_cluster_changes(doc_id, &old_terms, &BTreeMap::new(), &[])?;
        let mut totals = Vec::with_capacity(old_lengths.len());
        for (field, length) in &old_lengths {
            let base = self
                .store
                .get(&field_stats_key(&self.table, field)?)?
                .map(|value| decode_u64_value(&value))
                .transpose()?
                .unwrap_or(0);
            totals.push((
                field.clone(),
                base.checked_sub(*length).ok_or_else(|| {
                    other_error("stored field length is smaller than removed document length")
                })?,
            ));
        }

        let mut batch = self.store.batch();
        Self::apply_cluster_changes(batch.as_mut(), &self.table, cluster_changes)?;
        batch.delete_prefix(&posting_document_doc_prefix(&self.table, doc_id)?)?;
        for (field, total) in totals {
            Self::set_total_length(batch.as_mut(), &self.table, &field, total)?;
            batch.delete(&doc_length_key(&self.table, doc_id, &field)?)?;
        }
        batch.commit()
    }

    fn try_rebuild_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        let mut staged = BTreeMap::new();
        for (doc_id, fields) in documents {
            if !fields.is_empty() {
                staged.insert(doc_id, self.analyze_fields(fields)?);
            }
        }
        let mut totals = BTreeMap::<FieldName, u64>::new();
        let mut clusters = BTreeMap::<ClusterKey, Vec<ClusterPosting>>::new();
        for (doc_id, (lengths, postings)) in &staged {
            for (field, length) in lengths {
                let total = totals.entry(field.clone()).or_default();
                *total = total
                    .checked_add(*length)
                    .ok_or_else(|| other_error("total field length overflow"))?;
            }
            for (field, term, positions) in postings {
                clusters
                    .entry((field.clone(), term.clone(), cluster_id(*doc_id)))
                    .or_default()
                    .push(ClusterPosting {
                        doc_id: *doc_id,
                        term_freq: positions.len() as u64,
                        doc_length: lengths[field],
                        positions: positions.clone(),
                    });
            }
        }

        let mut batch = self.store.batch();
        batch.delete_prefix(&posting_cluster_score_key_prefix(&self.table)?)?;
        batch.delete_prefix(&posting_cluster_positions_key_prefix(&self.table)?)?;
        batch.delete_prefix(&posting_document_key_prefix(&self.table)?)?;
        batch.delete_prefix(&doc_length_key_prefix(&self.table)?)?;
        batch.delete_prefix(&field_stats_key_prefix(&self.table)?)?;
        for (field, total) in totals {
            Self::set_total_length(batch.as_mut(), &self.table, &field, total)?;
        }
        for ((field, term, posting_cluster), entries) in clusters {
            let (score, positions) = encode_cluster(&entries)?;
            batch.put(
                &posting_cluster_score_key(&self.table, &field, &term, posting_cluster)?,
                &score,
            )?;
            batch.put(
                &posting_cluster_positions_key(&self.table, &field, &term, posting_cluster)?,
                &positions,
            )?;
        }
        for (doc_id, (lengths, postings)) in staged {
            for (field, length) in lengths {
                batch.put(
                    &doc_length_key(&self.table, doc_id, &field)?,
                    &u64_value(length),
                )?;
                let terms = postings
                    .iter()
                    .filter(|(posting_field, _, _)| posting_field == &field)
                    .map(|(_, term, _)| term.clone())
                    .collect::<Vec<_>>();
                batch.put(
                    &posting_document_key(&self.table, doc_id, &field)?,
                    &encode_terms(&terms)?,
                )?;
            }
        }
        batch.commit()
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete_prefix(&posting_cluster_score_key_prefix(&self.table)?)?;
        batch.delete_prefix(&posting_cluster_positions_key_prefix(&self.table)?)?;
        batch.delete_prefix(&posting_document_key_prefix(&self.table)?)?;
        batch.delete_prefix(&doc_length_key_prefix(&self.table)?)?;
        batch.delete_prefix(&field_stats_key_prefix(&self.table)?)?;
        batch.commit()
    }

    fn get_posting_list(&self, field: &str, term: &str) -> StorageBackendResult<PostingList> {
        let mut entries = Vec::new();
        for (key, score) in self.store.scan_prefix(&posting_cluster_score_term_prefix(
            &self.table,
            field,
            term,
        )?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _field = read_str(&key, &mut offset)?;
            let _term = read_str(&key, &mut offset)?;
            let posting_cluster = read_u64(&key, &mut offset)?;
            let positions = self
                .store
                .get(&posting_cluster_positions_key(
                    &self.table,
                    field,
                    term,
                    posting_cluster,
                )?)?
                .ok_or_else(|| other_error("clustered posting positions value is missing"))?;
            entries.extend(
                decode_cluster(posting_cluster, &score, &positions)?
                    .into_iter()
                    .map(|entry| {
                        PostingEntry::new(
                            entry.doc_id,
                            Payload {
                                positions: entry.positions,
                                score: 0.0,
                                fields: BTreeMap::new(),
                            },
                        )
                    }),
            );
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn posting_cursor(
        &self,
        field: &str,
        term: &str,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        self.cursor_for_term(field, term)
    }

    fn for_each_term_freq(
        &self,
        field: &str,
        term: &str,
        visit: &mut dyn FnMut(DocId, u64),
    ) -> StorageBackendResult<()> {
        let mut cursor = self.cursor_for_term(field, term)?;
        while let Some(entry) = cursor.current() {
            visit(entry.doc_id, entry.term_freq);
            cursor.advance()?;
        }
        Ok(())
    }

    fn doc_freq(&self, field: &str, term: &str) -> StorageBackendResult<u64> {
        self.store
            .scan_prefix(&posting_cluster_score_term_prefix(
                &self.table,
                field,
                term,
            )?)?
            .into_iter()
            .try_fold(0_u64, |total, (_, score)| {
                total
                    .checked_add(score_count(&score)?)
                    .ok_or_else(|| other_error("document frequency overflow"))
            })
    }

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> StorageBackendResult<u64> {
        Ok(self
            .store
            .get(&doc_length_key(&self.table, doc_id, field)?)?
            .map(|value| decode_u64_value(&value))
            .transpose()?
            .unwrap_or(0))
    }

    fn get_scoring_inputs_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<(u64, Vec<u64>)>> {
        let mut output = doc_ids
            .iter()
            .map(|doc_id| Ok((self.get_doc_length(*doc_id, field)?, vec![0; terms.len()])))
            .collect::<StorageBackendResult<Vec<_>>>()?;
        let mut positions = BTreeMap::<DocId, Vec<usize>>::new();
        for (position, doc_id) in doc_ids.iter().copied().enumerate() {
            positions.entry(doc_id).or_default().push(position);
        }
        for (term_index, term) in terms.iter().enumerate() {
            let mut cursor = self.cursor_for_term(field, term)?;
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
        let posting_cluster = cluster_id(doc_id);
        self.store
            .get(&posting_cluster_score_key(
                &self.table,
                field,
                term,
                posting_cluster,
            )?)?
            .map_or(Ok(0), |score| {
                let entries = decode_all_scores(posting_cluster, &score)?;
                Ok(entries
                    .binary_search_by_key(&doc_id, |entry| entry.doc_id)
                    .ok()
                    .map_or(0, |position| entries[position].term_freq))
            })
    }

    fn doc_count(&self) -> StorageBackendResult<u64> {
        let mut doc_ids = BTreeSet::new();
        for (key, _) in self
            .store
            .scan_prefix(&doc_length_key_prefix(&self.table)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            doc_ids.insert(read_u64(&key, &mut offset)?);
        }
        usize_to_u64(doc_ids.len(), "document count")
    }

    fn total_field_length(&self, field: &str) -> StorageBackendResult<u64> {
        Ok(self
            .store
            .get(&field_stats_key(&self.table, field)?)?
            .map(|value| decode_u64_value(&value))
            .transpose()?
            .unwrap_or(0))
    }

    fn vocabulary_terms(&self, field: &str) -> StorageBackendResult<Vec<String>> {
        let mut terms = BTreeSet::new();
        for (key, _) in self
            .store
            .scan_prefix(&posting_cluster_score_field_prefix(&self.table, field)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _field = read_str(&key, &mut offset)?;
            terms.insert(read_str(&key, &mut offset)?);
        }
        Ok(terms.into_iter().collect())
    }

    fn stats(&self) -> StorageBackendResult<IndexStats> {
        let doc_count = self.doc_count()?;
        let mut stats = IndexStats::default();
        stats.total_docs = doc_count;
        if doc_count > 0 {
            let mut total = 0_u64;
            for (_, value) in self
                .store
                .scan_prefix(&field_stats_key_prefix(&self.table)?)?
            {
                total = total
                    .checked_add(decode_u64_value(&value)?)
                    .ok_or_else(|| other_error("index total field length overflow"))?;
            }
            stats.avg_doc_length = total as f64 / doc_count as f64;
        }
        let mut counts = BTreeMap::<(String, String), u64>::new();
        for (key, value) in self
            .store
            .scan_prefix(&posting_cluster_score_key_prefix(&self.table)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            let term = read_str(&key, &mut offset)?;
            let count = counts.entry((field, term)).or_default();
            *count = count
                .checked_add(score_count(&value)?)
                .ok_or_else(|| other_error("index document frequency overflow"))?;
        }
        for ((field, term), document_frequency) in counts {
            stats.set_doc_freq(field, term, document_frequency);
        }
        Ok(stats)
    }

    fn posting_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        let prefix = match field {
            Some(field) => posting_cluster_score_field_prefix(&self.table, field)?,
            None => posting_cluster_score_key_prefix(&self.table)?,
        };
        self.store
            .scan_prefix(&prefix)?
            .into_iter()
            .try_fold(0_u64, |total, (_, value)| {
                total
                    .checked_add(score_count(&value)?)
                    .ok_or_else(|| other_error("posting count overflow"))
            })
    }

    fn doc_length_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        let mut count = 0_u64;
        for (key, _) in self
            .store
            .scan_prefix(&doc_length_key_prefix(&self.table)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            let indexed_field = read_str(&key, &mut offset)?;
            if field.is_none_or(|target| target == indexed_field) {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| other_error("document-length row count overflow"))?;
            }
        }
        Ok(count)
    }

    fn term_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        let prefix = match field {
            Some(field) => posting_cluster_score_field_prefix(&self.table, field)?,
            None => posting_cluster_score_key_prefix(&self.table)?,
        };
        let mut terms = BTreeSet::new();
        for (key, _) in self.store.scan_prefix(&prefix)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let current_field = read_str(&key, &mut offset)?;
            let term = read_str(&key, &mut offset)?;
            terms.insert((current_field, term));
        }
        usize_to_u64(terms.len(), "term count")
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        Ok(Arc::new(self.clone()))
    }

    fn field_names(&self) -> StorageBackendResult<Vec<FieldName>> {
        let mut fields = Vec::new();
        for (key, _) in self
            .store
            .scan_prefix(&field_stats_key_prefix(&self.table)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            fields.push(read_str(&key, &mut offset)?);
        }
        Ok(fields)
    }

    fn set_field_analyzer(
        &mut self,
        field: &str,
        analyzer: Analyzer,
        phase: AnalyzerPhase,
    ) -> Result<(), String> {
        crate::inverted_index::validate_linear_analyzer(&analyzer)
            .map_err(|error| error.to_string())?;
        self.bindings
            .bind(field, &analyzer, phase)
            .map_err(|error| error.to_string())
    }

    fn remove_field_analyzers(&mut self, field: &str) -> Result<(), String> {
        self.bindings.remove(field);
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
        crate::inverted_index::validate_linear_revision(&revision)
            .map_err(|error| error.to_string())?;
        self.bindings
            .bind_revision(field, revision, phase)
            .map_err(|error| error.to_string())
    }

    fn set_field_analyzer_revisions(
        &mut self,
        field: &str,
        index: Arc<uqa_analysis::CompiledAnalyzer>,
        search: Arc<uqa_analysis::CompiledAnalyzer>,
    ) -> Result<(), String> {
        crate::inverted_index::validate_linear_revision(&index)
            .map_err(|error| error.to_string())?;
        crate::inverted_index::validate_linear_revision(&search)
            .map_err(|error| error.to_string())?;
        let mut candidate = self.bindings.clone();
        candidate
            .bind_revisions(field, index, search)
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
        replacement
            .set_field_analyzer_revision(field, revision, phase)
            .map_err(crate::StorageBackendError::Other)?;
        replacement.try_rebuild_documents(documents)?;
        *self = replacement;
        Ok(())
    }
}
