//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `InvertedIndex` trait implementation and read/statistics surface.

use super::queries::project_postings;
use super::{
    clustered_result, decode_index_u64, encode_index_u64, invalidate_posting_accelerators, params,
    params_from_iter, quote_ident, Analyzer, AnalyzerPhase, Arc, BTreeMap, BlockMaxScorer, DocId,
    FieldName, IndexStats, InvertedIndex, OptionalExtension, PostingCursor, PostingList,
    SQLiteError, SQLiteInvertedIndex, SqlValue, StorageBackendResult,
};
use super::{IndexedFieldMetadata, TokenTermKey};
use uqa_storage::clustered_postings::{cluster_id, decode_all_scores, OccurrencePosting};

impl InvertedIndex for SQLiteInvertedIndex {
    fn analyzer(&self) -> &Analyzer {
        self.bindings.default_configuration()
    }

    fn add_document(
        &mut self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        Ok(self.add_document_inner(doc_id, fields)?)
    }

    fn try_add_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        Ok(self.add_documents_inner(documents)?)
    }

    fn remove_document(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        Ok(self.remove_document_inner(doc_id)?)
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            invalidate_posting_accelerators(&tx, &self.table)?;
            self.clear_index_on(&tx)?;
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    fn source_rebuild_required(&self) -> StorageBackendResult<bool> {
        Ok(self.conn.with(|conn| self.needs_source_rebuild_on(conn))?)
    }

    fn try_rebuild_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        Ok(self.rebuild_documents_inner(documents)?)
    }

    fn get_posting_list(&self, field: &str, term: &str) -> StorageBackendResult<PostingList> {
        self.get_posting_list_key(field, &TokenTermKey::from_text(term))
    }

    fn get_posting_list_key(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<PostingList> {
        Ok(project_postings(self.get_occurrence_postings(field, term)?))
    }

    fn get_occurrence_postings(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Vec<OccurrencePosting>> {
        Ok(self
            .occurrence_postings_bulk(field, std::slice::from_ref(term))?
            .pop()
            .unwrap_or_default())
    }

    fn get_occurrences(
        &self,
        doc_id: DocId,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Vec<uqa_core::TokenOccurrence>> {
        Ok(self.conn.with(|conn| {
            self.require_graph_format_on(conn)?;
            let entries = super::load_cluster(conn, &self.table, field, term, cluster_id(doc_id))?;
            let Some(posting) = entries.into_iter().find(|entry| entry.doc_id == doc_id) else {
                return Ok(Vec::new());
            };
            self.validate_posting_metadata_on(conn, field, &posting)?;
            Ok(posting.occurrences)
        })?)
    }

    fn indexed_field_metadata(
        &self,
        doc_id: DocId,
        field: &str,
    ) -> StorageBackendResult<Option<IndexedFieldMetadata>> {
        let doc_id = encode_index_u64("document", doc_id)?;
        Ok(self.conn.with(|conn| {
            self.require_graph_format_on(conn)?;
            self.read_field_metadata_on(conn, doc_id, field)
        })?)
    }

    fn get_posting_lists_bulk(
        &self,
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<PostingList>> {
        let keys = terms
            .iter()
            .map(|term| TokenTermKey::from_text(term))
            .collect::<Vec<_>>();
        Ok(self
            .occurrence_postings_bulk(field, &keys)?
            .into_iter()
            .map(project_postings)
            .collect())
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
        self.cursor_for_term(field, term)
    }

    fn posting_cursors_bulk(
        &self,
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<Box<dyn PostingCursor>>> {
        let keys = terms
            .iter()
            .map(|term| TokenTermKey::from_text(term))
            .collect::<Vec<_>>();
        self.cursors_for_terms(field, &keys)
    }

    fn posting_cursors_keys_bulk(
        &self,
        field: &str,
        terms: &[TokenTermKey],
    ) -> StorageBackendResult<Vec<Box<dyn PostingCursor>>> {
        self.cursors_for_terms(field, terms)
    }

    fn get_posting_lists_keys_bulk(
        &self,
        field: &str,
        terms: &[TokenTermKey],
    ) -> StorageBackendResult<Vec<PostingList>> {
        Ok(self
            .occurrence_postings_bulk(field, terms)?
            .into_iter()
            .map(project_postings)
            .collect())
    }

    fn persisted_block_max_scores_keys_bulk(
        &self,
        field: &str,
        terms: &[TokenTermKey],
        scorer_fingerprint: &str,
    ) -> StorageBackendResult<Vec<Option<Vec<f64>>>> {
        if scorer_fingerprint.is_empty() {
            return Ok(vec![None; terms.len()]);
        }
        self.get_versioned_block_max_scores_keys_bulk(field, terms, scorer_fingerprint)
    }

    fn rebuild_persisted_block_max(
        &mut self,
        field: &str,
        scorer: &dyn BlockMaxScorer,
        scorer_fingerprint: &str,
    ) -> StorageBackendResult<bool> {
        if scorer_fingerprint.is_empty() {
            return Err(SQLiteError::StorageBackend(
                "persisted block-max scorer fingerprint must not be empty".into(),
            )
            .into());
        }
        let terms = self.vocabulary_keys(field)?;
        self.ensure_aux_tables(field)?;
        let table = self.blockmax_table_name(field);
        self.conn.with_mut(|conn| {
            conn.execute(&format!("DELETE FROM {}", quote_ident(&table)), [])?;
            Ok(())
        })?;
        for term in terms {
            self.build_block_max_scores_key(field, &term, scorer, scorer_fingerprint)?;
        }
        Ok(true)
    }

    fn persisted_block_max_scores(
        &self,
        field: &str,
        term: &str,
        scorer_fingerprint: &str,
    ) -> StorageBackendResult<Option<Vec<f64>>> {
        if scorer_fingerprint.is_empty() {
            return Ok(None);
        }
        self.get_versioned_block_max_scores(field, term, scorer_fingerprint)
    }

    fn persisted_block_max_scores_bulk(
        &self,
        field: &str,
        terms: &[String],
        scorer_fingerprint: &str,
    ) -> StorageBackendResult<Vec<Option<Vec<f64>>>> {
        if scorer_fingerprint.is_empty() {
            return Ok(vec![None; terms.len()]);
        }
        self.get_versioned_block_max_scores_bulk(field, terms, scorer_fingerprint)
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
        Ok(self.posting_cursor_key(field, term)?.doc_freq())
    }

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> StorageBackendResult<u64> {
        Ok(self
            .get_doc_lengths_bulk(&[doc_id], field)?
            .get(&doc_id)
            .copied()
            .unwrap_or(0))
    }

    fn get_doc_lengths_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
    ) -> StorageBackendResult<BTreeMap<DocId, u64>> {
        Ok(self.conn.with(|conn| {
            self.require_graph_format_on(conn)?;
            let mut out = BTreeMap::new();
            for chunk in doc_ids.chunks(900) {
                let ids = (3..chunk.len()+3).map(|parameter| format!("?{parameter}")).collect::<Vec<_>>().join(", ");
                let sql = format!("WITH lengths AS (SELECT doc_id, length FROM _occurrence_lengths WHERE table_name = ?1 AND field = ?2 AND doc_id IN ({ids})), documents AS (SELECT doc_id, metadata_blob FROM _occurrence_documents WHERE table_name = ?1 AND field = ?2 AND doc_id IN ({ids})) SELECT COALESCE(lengths.doc_id, documents.doc_id), length, metadata_blob FROM lengths FULL OUTER JOIN documents USING(doc_id)");
                let mut values = vec![SqlValue::Text(self.table.clone()), SqlValue::Text(field.into())];
                for doc_id in chunk {
                    values.push(SqlValue::Integer(encode_index_u64("document", *doc_id)?));
                }
                let mut statement = conn.prepare(&sql)?;
                let rows = statement.query_map(params_from_iter(values), |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, Option<Vec<u8>>>(2)?)))?;
                for row in rows {
                    let (doc_id, length, metadata) = row?;
                    let doc_id = decode_index_u64("document id", doc_id)?;
                    let length = length.map(|length| decode_index_u64("document length", length)).transpose()?;
                    let metadata = metadata.map(|bytes| clustered_result(IndexedFieldMetadata::from_bytes(&bytes))).transpose()?;
                    let (Some(length), Some(metadata)) = (length, metadata) else {
                        return Err(SQLiteError::StorageBackend("indexed field length and source metadata disagree".into()));
                    };
                    let stats = self.stored_field_stats_on(conn, field)?.ok_or_else(|| SQLiteError::StorageBackend("indexed field revision is missing".into()))?;
                    if metadata.length != length || metadata.revision() != stats.revision {
                        return Err(SQLiteError::StorageBackend("indexed field length or revision disagrees with source metadata".into()));
                    }
                    out.insert(doc_id, length);
                }
            }
            Ok(out)
        })?)
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
        if doc_ids.is_empty() {
            return Ok(Vec::new());
        }

        let doc_lengths = self.get_doc_lengths_bulk(doc_ids, field)?;
        let mut inputs: Vec<(u64, Vec<u64>)> = doc_ids
            .iter()
            .map(|doc_id| {
                (
                    doc_lengths.get(doc_id).copied().unwrap_or(0),
                    vec![0; terms.len()],
                )
            })
            .collect();
        if terms.is_empty() {
            return Ok(inputs);
        }

        let mut output_positions = BTreeMap::<DocId, Vec<usize>>::new();
        for (position, doc_id) in doc_ids.iter().copied().enumerate() {
            output_positions.entry(doc_id).or_default().push(position);
        }
        for (term_index, mut cursor) in self
            .posting_cursors_keys_bulk(field, terms)?
            .into_iter()
            .enumerate()
        {
            while let Some(entry) = cursor.current() {
                if let Some(positions) = output_positions.get(&entry.doc_id) {
                    for position in positions {
                        inputs[*position].1[term_index] = entry.term_freq;
                        inputs[*position].0 = entry.doc_length;
                    }
                }
                cursor.advance()?;
            }
        }
        Ok(inputs)
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
        let cluster = encode_index_u64("posting cluster", cluster_id(doc_id))?;
        Ok(self.conn.with(|conn| {
            self.require_graph_format_on(conn)?;
            let row: Option<(i64, Vec<u8>)> = conn.query_row("SELECT posting_count, score_blob FROM _occurrence_clusters WHERE table_name = ?1 AND field = ?2 AND term = ?3 AND cluster_id = ?4", params![self.table, field, term.as_bytes(), cluster], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
            match row {
                Some((count, bytes)) => {
                    // Validate the version and redundant row count before decoding only the requested cluster.
                    super::posting_cursor_from_rows(vec![(cluster, count, bytes.clone())])?;
                    let scores = clustered_result(decode_all_scores(cluster_id(doc_id), &bytes))?;
                    Ok(scores.binary_search_by_key(&doc_id, |entry| entry.doc_id).ok().map_or(0, |position| scores[position].term_freq))
                }
                None => Ok(0),
            }
        })?)
    }

    fn doc_count(&self) -> StorageBackendResult<u64> {
        Ok(self.conn.with(|c| {
            self.require_graph_format_on(c)?;
            let n: i64 = c.query_row(
                "SELECT COUNT(DISTINCT doc_id) FROM _occurrence_lengths
                     WHERE table_name = ?1",
                params![self.table],
                |r| r.get(0),
            )?;
            decode_index_u64("document count", n)
        })?)
    }

    fn total_field_length(&self, field: &str) -> StorageBackendResult<u64> {
        Ok(self.conn.with(|conn| {
            self.require_graph_format_on(conn)?;
            Ok(self
                .stored_field_stats_on(conn, field)?
                .map_or(0, |stats| stats.total_length))
        })?)
    }

    fn vocabulary_terms(&self, field: &str) -> StorageBackendResult<Vec<String>> {
        self.terms_for_field(field)
    }

    fn vocabulary_keys(&self, field: &str) -> StorageBackendResult<Vec<TokenTermKey>> {
        Ok(self.conn.with(|conn| {
            self.require_graph_format_on(conn)?;
            let mut statement = conn.prepare("SELECT DISTINCT term FROM _occurrence_clusters WHERE table_name = ?1 AND field = ?2 ORDER BY term")?;
            let rows = statement.query_map(params![self.table, field], |row| row.get::<_, Vec<u8>>(0))?;
            rows.map(|row| clustered_result(TokenTermKey::from_bytes(row?))).collect()
        })?)
    }

    fn field_doc_count(&self, field: &str) -> StorageBackendResult<u64> {
        Ok(self.conn.with(|conn| {
            self.require_graph_format_on(conn)?;
            Ok(self
                .stored_field_stats_on(conn, field)?
                .map_or(0, |stats| stats.doc_count))
        })?)
    }

    fn stats(&self) -> StorageBackendResult<IndexStats> {
        let doc_count = self.doc_count()?;
        let mut s = IndexStats::default();
        s.total_docs = doc_count;
        if doc_count > 0 {
            let total: u64 = self.conn.with(|c| {
                self.require_graph_format_on(c)?;
                let n: i64 = c.query_row(
                    "SELECT COALESCE(SUM(total_length), 0) FROM _occurrence_fields
                         WHERE table_name = ?1",
                    params![self.table],
                    |r| r.get(0),
                )?;
                decode_index_u64("total indexed length", n)
            })?;
            s.avg_doc_length = total as f64 / doc_count as f64;
        }
        let pairs = self
            .conn
            .with(|conn| self.term_frequencies_on(conn, None))?;
        for ((field, term), df) in pairs {
            let term = term.to_term();
            if let Some(text) = term.as_str() {
                s.set_doc_freq(field, text, df);
            } else {
                s.set_doc_freq_utf16(field, term.into_utf16(), df);
            }
        }
        Ok(s)
    }

    fn posting_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        Ok(self.conn.with(|conn| {
            self.term_frequencies_on(conn, field)?
                .into_values()
                .try_fold(0_u64, |total, count| {
                    total
                        .checked_add(count)
                        .ok_or_else(|| SQLiteError::StorageBackend("posting count overflow".into()))
                })
        })?)
    }

    fn doc_length_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        Ok(self.conn.with(|c| {
            self.require_graph_format_on(c)?;
            let n: i64 = if let Some(field) = field {
                c.query_row(
                    "SELECT COUNT(*) FROM _occurrence_lengths
                         WHERE table_name = ?1 AND field = ?2",
                    params![self.table, field],
                    |r| r.get(0),
                )?
            } else {
                c.query_row(
                    "SELECT COUNT(*) FROM _occurrence_lengths WHERE table_name = ?1",
                    params![self.table],
                    |r| r.get(0),
                )?
            };
            decode_index_u64("document length count", n)
        })?)
    }

    fn term_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        Ok(self.conn.with(|conn| {
            let terms = self
                .term_frequencies_on(conn, field)?
                .into_keys()
                .map(|(_, term)| term)
                .collect::<std::collections::BTreeSet<_>>();
            Ok(terms.len() as u64)
        })?)
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        Ok(Arc::new(self.clone()))
    }

    fn field_names(&self) -> StorageBackendResult<Vec<FieldName>> {
        Ok(self.conn.with(|c| {
            self.require_graph_format_on(c)?;
            let mut stmt =
                c.prepare("SELECT DISTINCT field FROM _occurrence_lengths WHERE table_name = ?1")?;
            let rows = stmt.query_map([&self.table], |row| row.get::<_, String>(0))?;
            let mut fields = Vec::new();
            for row in rows {
                fields.push(row?);
            }
            Ok(fields)
        })?)
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
        replacement.rebuild_documents_inner(documents)?;
        *self = replacement;
        Ok(())
    }
}
