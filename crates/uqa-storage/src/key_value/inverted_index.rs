//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inverted-index adapter over an ordered key/value store.

use super::codec::{
    blob_to_positions, decode_u64_value, doc_length_doc_prefix, doc_length_key,
    doc_length_key_prefix, field_stats_key, field_stats_key_prefix, other_error, positions_to_blob,
    posting_key, posting_key_prefix, posting_term_prefix, read_str, read_u64,
    reverse_posting_doc_prefix, reverse_posting_key, reverse_posting_key_prefix, u64_value,
    usize_to_u64,
};
use super::{
    Analyzer, AnalyzerPhase, Arc, BTreeMap, BTreeSet, DocId, FieldName, IndexStats, InvertedIndex,
    KeyValueBatch, KeyValueStore, Payload, PostingEntry, PostingList, StorageBackendResult,
};

/// Inverted index implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueInvertedIndex {
    store: Arc<dyn KeyValueStore>,
    table: String,
    analyzer: Analyzer,
    index_field_analyzers: BTreeMap<FieldName, Analyzer>,
    search_field_analyzers: BTreeMap<FieldName, Analyzer>,
}

type KeyValueStagedPosting = (FieldName, String, Vec<u32>);
type KeyValueAnalyzedFields = (BTreeMap<FieldName, u64>, Vec<KeyValueStagedPosting>);

impl KeyValueInvertedIndex {
    pub fn new(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        analyzer: Analyzer,
    ) -> Self {
        Self {
            store,
            table: table.into(),
            analyzer,
            index_field_analyzers: BTreeMap::new(),
            search_field_analyzers: BTreeMap::new(),
        }
    }

    fn old_doc_lengths(&self, doc_id: DocId) -> StorageBackendResult<BTreeMap<FieldName, u64>> {
        let mut out = BTreeMap::new();
        for (key, value) in self
            .store
            .scan_prefix(&doc_length_doc_prefix(&self.table, doc_id)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            out.insert(field, decode_u64_value(&value)?);
        }
        Ok(out)
    }

    fn old_terms(&self, doc_id: DocId) -> StorageBackendResult<Vec<(FieldName, String)>> {
        let mut out = Vec::new();
        for (key, _) in self
            .store
            .scan_prefix(&reverse_posting_doc_prefix(&self.table, doc_id)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            let term = read_str(&key, &mut offset)?;
            out.push((field, term));
        }
        Ok(out)
    }

    fn analyze_fields(
        &self,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<KeyValueAnalyzedFields> {
        let mut lengths = BTreeMap::new();
        let mut postings = Vec::new();
        for (field, text) in fields {
            let analyzer = self
                .index_field_analyzers
                .get(&field)
                .unwrap_or(&self.analyzer);
            let tokens = analyzer.analyze(&text)?;
            let token_count = usize_to_u64(tokens.len(), "document token count")?;
            crate::inverted_index::validate_token_position_count(token_count)?;
            lengths.insert(field.clone(), token_count);
            let mut term_positions: BTreeMap<String, Vec<u32>> = BTreeMap::new();
            for (pos, token) in tokens.into_iter().enumerate() {
                term_positions.entry(token).or_default().push(
                    u32::try_from(pos)
                        .map_err(|_| other_error("token position exceeds u32 index format"))?,
                );
            }
            for (term, mut positions) in term_positions {
                positions.sort_unstable();
                positions.dedup();
                postings.push((field.clone(), term, positions));
            }
        }
        Ok((lengths, postings))
    }

    fn set_total_length(
        batch: &mut dyn KeyValueBatch,
        table: &str,
        field: &str,
        value: u64,
    ) -> StorageBackendResult<()> {
        let key = field_stats_key(table, field)?;
        if value == 0 {
            batch.delete(&key)
        } else {
            batch.put(&key, &u64_value(value))
        }
    }
}

impl InvertedIndex for KeyValueInvertedIndex {
    fn analyzer(&self) -> &Analyzer {
        &self.analyzer
    }

    fn add_document(
        &mut self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        let old_lengths = self.old_doc_lengths(doc_id)?;
        let old_terms = self.old_terms(doc_id)?;

        let (new_lengths, new_postings) = self.analyze_fields(fields)?;

        let mut fields_to_update = BTreeSet::new();
        fields_to_update.extend(old_lengths.keys().cloned());
        fields_to_update.extend(new_lengths.keys().cloned());

        let mut batch = self.store.batch();
        for (field, term) in old_terms {
            batch.delete(&posting_key(&self.table, &field, &term, doc_id)?)?;
            batch.delete(&reverse_posting_key(&self.table, doc_id, &field, &term)?)?;
        }
        for field in old_lengths.keys() {
            batch.delete(&doc_length_key(&self.table, doc_id, field)?)?;
        }

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
            Self::set_total_length(batch.as_mut(), &self.table, &field, total)?;
        }

        for (field, length) in &new_lengths {
            batch.put(
                &doc_length_key(&self.table, doc_id, field)?,
                &u64_value(*length),
            )?;
        }
        for (field, term, positions) in new_postings {
            batch.put(
                &posting_key(&self.table, &field, &term, doc_id)?,
                &positions_to_blob(&positions)?,
            )?;
            batch.put(
                &reverse_posting_key(&self.table, doc_id, &field, &term)?,
                &[],
            )?;
        }
        batch.commit()
    }

    fn remove_document(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let old_lengths = self.old_doc_lengths(doc_id)?;
        let old_terms = self.old_terms(doc_id)?;
        let mut batch = self.store.batch();
        for (field, term) in old_terms {
            batch.delete(&posting_key(&self.table, &field, &term, doc_id)?)?;
            batch.delete(&reverse_posting_key(&self.table, doc_id, &field, &term)?)?;
        }
        for (field, length) in old_lengths {
            let base = self
                .store
                .get(&field_stats_key(&self.table, &field)?)?
                .map(|value| decode_u64_value(&value))
                .transpose()?
                .unwrap_or(0);
            let total = base.checked_sub(length).ok_or_else(|| {
                other_error("stored field length is smaller than removed document length")
            })?;
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

        let mut totals: BTreeMap<FieldName, u64> = BTreeMap::new();
        for (lengths, _) in staged.values() {
            for (field, length) in lengths {
                let total = totals.entry(field.clone()).or_insert(0);
                *total = total
                    .checked_add(*length)
                    .ok_or_else(|| other_error("total field length overflow"))?;
            }
        }

        let mut batch = self.store.batch();
        batch.delete_prefix(&posting_key_prefix(&self.table)?)?;
        batch.delete_prefix(&doc_length_key_prefix(&self.table)?)?;
        batch.delete_prefix(&field_stats_key_prefix(&self.table)?)?;
        batch.delete_prefix(&reverse_posting_key_prefix(&self.table)?)?;
        for (field, total) in totals {
            Self::set_total_length(batch.as_mut(), &self.table, &field, total)?;
        }
        for (doc_id, (lengths, postings)) in staged {
            for (field, length) in lengths {
                batch.put(
                    &doc_length_key(&self.table, doc_id, &field)?,
                    &u64_value(length),
                )?;
            }
            for (field, term, positions) in postings {
                batch.put(
                    &posting_key(&self.table, &field, &term, doc_id)?,
                    &positions_to_blob(&positions)?,
                )?;
                batch.put(
                    &reverse_posting_key(&self.table, doc_id, &field, &term)?,
                    &[],
                )?;
            }
        }
        batch.commit()
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete_prefix(&posting_key_prefix(&self.table)?)?;
        batch.delete_prefix(&doc_length_key_prefix(&self.table)?)?;
        batch.delete_prefix(&field_stats_key_prefix(&self.table)?)?;
        batch.delete_prefix(&reverse_posting_key_prefix(&self.table)?)?;
        batch.commit()
    }

    fn get_posting_list(&self, field: &str, term: &str) -> StorageBackendResult<PostingList> {
        let mut entries = Vec::new();
        for (key, value) in
            self.store
                .scan_prefix(&posting_term_prefix(&self.table, field, term)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _field = read_str(&key, &mut offset)?;
            let _term = read_str(&key, &mut offset)?;
            let doc_id = read_u64(&key, &mut offset)?;
            entries.push(PostingEntry::new(
                doc_id,
                Payload {
                    positions: blob_to_positions(&value)?,
                    score: 0.0,
                    fields: BTreeMap::new(),
                },
            ));
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn doc_freq(&self, field: &str, term: &str) -> StorageBackendResult<u64> {
        usize_to_u64(
            self.store
                .scan_prefix(&posting_term_prefix(&self.table, field, term)?)?
                .len(),
            "document frequency",
        )
    }

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> StorageBackendResult<u64> {
        Ok(self
            .store
            .get(&doc_length_key(&self.table, doc_id, field)?)?
            .map(|value| decode_u64_value(&value))
            .transpose()?
            .unwrap_or(0))
    }

    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> StorageBackendResult<u64> {
        self.store
            .get(&posting_key(&self.table, field, term, doc_id)?)?
            .map_or(Ok(0), |value| {
                blob_to_positions(&value)
                    .and_then(|positions| usize_to_u64(positions.len(), "term frequency"))
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
        for (key, _) in self.store.scan_prefix(&posting_key_prefix(&self.table)?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let indexed_field = read_str(&key, &mut offset)?;
            let term = read_str(&key, &mut offset)?;
            if indexed_field == field {
                terms.insert(term);
            }
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
        let mut counts: BTreeMap<(String, String), u64> = BTreeMap::new();
        for (key, _) in self.store.scan_prefix(&posting_key_prefix(&self.table)?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            let term = read_str(&key, &mut offset)?;
            let count = counts.entry((field, term)).or_insert(0);
            *count = count
                .checked_add(1)
                .ok_or_else(|| other_error("index document frequency overflow"))?;
        }
        for ((field, term), df) in counts {
            stats.set_doc_freq(field, term, df);
        }
        Ok(stats)
    }

    fn posting_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        let mut count = 0_u64;
        for (key, _) in self.store.scan_prefix(&posting_key_prefix(&self.table)?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let indexed_field = read_str(&key, &mut offset)?;
            let _term = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            if field.is_none_or(|target| target == indexed_field) {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| other_error("posting count overflow"))?;
            }
        }
        Ok(count)
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
        let mut terms = BTreeSet::new();
        for (key, _) in self.store.scan_prefix(&posting_key_prefix(&self.table)?)? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let current_field = read_str(&key, &mut offset)?;
            let term = read_str(&key, &mut offset)?;
            if field.is_none_or(|target| target == current_field) {
                terms.insert((current_field, term));
            }
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
        if let Some(analyzer) = self.search_field_analyzers.get(field) {
            return analyzer.clone();
        }
        if let Some(analyzer) = self.index_field_analyzers.get(field) {
            return analyzer.clone();
        }
        self.analyzer.clone()
    }
}
