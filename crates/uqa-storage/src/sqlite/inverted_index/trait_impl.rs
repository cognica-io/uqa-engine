//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `InvertedIndex` trait implementation and read/statistics surface.

use super::{
    clustered_result, decode_index_u64, encode_index_u64, invalidate_block_max_tables, params,
    params_from_iter, posting_cursor_from_rows, quote_ident, Analyzer, AnalyzerPhase, Arc,
    BTreeMap, BTreeSet, BlockMaxScorer, DocId, FieldName, IndexStats, InvertedIndex,
    OptionalExtension, Payload, PostingCursor, PostingEntry, PostingList, SQLiteError,
    SQLiteInvertedIndex, SqlValue, StorageBackendResult,
};
use crate::clustered_postings::{cluster_id, decode_all_scores, decode_cluster};

impl InvertedIndex for SQLiteInvertedIndex {
    fn analyzer(&self) -> &Analyzer {
        &self.analyzer
    }

    fn add_document(
        &mut self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> StorageBackendResult<()> {
        Ok(self.add_document_inner(doc_id, fields)?)
    }

    fn remove_document(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        Ok(self.remove_document_inner(doc_id)?)
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            invalidate_block_max_tables(&tx, &self.table)?;
            tx.execute(
                "DELETE FROM _posting_clusters WHERE table_name = ?1",
                params![self.table],
            )?;
            tx.execute(
                "DELETE FROM _posting_documents WHERE table_name = ?1",
                params![self.table],
            )?;
            tx.execute(
                "DELETE FROM _doc_lengths WHERE table_name = ?1",
                params![self.table],
            )?;
            tx.execute(
                "DELETE FROM _field_stats WHERE table_name = ?1",
                params![self.table],
            )?;
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    fn try_rebuild_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        Ok(self.rebuild_documents_inner(documents)?)
    }

    fn get_posting_list(&self, field: &str, term: &str) -> StorageBackendResult<PostingList> {
        Ok(self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT cluster_id, posting_count, score_blob, positions_blob
                   FROM _posting_clusters
                     WHERE table_name = ?1 AND field = ?2 AND term = ?3
                     ORDER BY cluster_id",
            )?;
            let rows = stmt.query_map(params![self.table, field, term], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                ))
            })?;
            let mut entries = Vec::new();
            for row in rows {
                let (stored_cluster, stored_count, score_blob, positions_blob) = row?;
                let posting_cluster = decode_index_u64("posting cluster", stored_cluster)?;
                let stored_count = decode_index_u64("posting count", stored_count)?;
                let decoded = clustered_result(decode_cluster(
                    posting_cluster,
                    &score_blob,
                    &positions_blob,
                ))?;
                if stored_count != decoded.len() as u64 {
                    return Err(SQLiteError::StorageBackend(
                        "corrupt clustered posting: stored posting count mismatch".into(),
                    ));
                }
                entries.extend(decoded.into_iter().map(|entry| {
                    PostingEntry::new(
                        entry.doc_id,
                        Payload {
                            positions: entry.positions,
                            score: 0.0,
                            fields: BTreeMap::new(),
                        },
                    )
                }));
            }
            Ok(PostingList::from_sorted_unchecked(entries))
        })?)
    }

    fn get_posting_lists_bulk(
        &self,
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<PostingList>> {
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let unique_terms = terms
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let posting_entries = self.conn.with(|c| {
            let mut by_term = BTreeMap::<String, Vec<PostingEntry>>::new();
            for chunk in unique_terms.chunks(900) {
                let placeholders = std::iter::repeat_n("?", chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "SELECT term, cluster_id, posting_count, score_blob, positions_blob
                       FROM _posting_clusters
                         WHERE table_name = ? AND field = ? AND term IN ({placeholders})
                         ORDER BY term, cluster_id"
                );
                let mut values = Vec::with_capacity(chunk.len() + 2);
                values.push(SqlValue::Text(self.table.clone()));
                values.push(SqlValue::Text(field.to_string()));
                values.extend(chunk.iter().cloned().map(SqlValue::Text));
                let mut stmt = c.prepare(&sql)?;
                let rows = stmt.query_map(params_from_iter(values), |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Vec<u8>>(3)?,
                        r.get::<_, Vec<u8>>(4)?,
                    ))
                })?;
                for row in rows {
                    let (term, stored_cluster, stored_count, score_blob, positions_blob) = row?;
                    let posting_cluster = decode_index_u64("posting cluster", stored_cluster)?;
                    let stored_count = decode_index_u64("posting count", stored_count)?;
                    let decoded = clustered_result(decode_cluster(
                        posting_cluster,
                        &score_blob,
                        &positions_blob,
                    ))?;
                    if stored_count != decoded.len() as u64 {
                        return Err(SQLiteError::StorageBackend(
                            "corrupt clustered posting: stored posting count mismatch".into(),
                        ));
                    }
                    by_term
                        .entry(term)
                        .or_default()
                        .extend(decoded.into_iter().map(|entry| {
                            PostingEntry::new(
                                entry.doc_id,
                                Payload {
                                    positions: entry.positions,
                                    score: 0.0,
                                    fields: BTreeMap::new(),
                                },
                            )
                        }));
                }
            }
            Ok(by_term)
        })?;

        Ok(terms
            .iter()
            .map(|term| {
                PostingList::from_sorted_unchecked(
                    posting_entries.get(term).cloned().unwrap_or_default(),
                )
            })
            .collect())
    }

    fn posting_cursor(
        &self,
        field: &str,
        term: &str,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        Ok(self.conn.with(|connection| {
            let mut statement = connection.prepare_cached(
                "SELECT cluster_id, posting_count, score_blob FROM _posting_clusters
                  WHERE table_name = ?1 AND field = ?2 AND term = ?3
                  ORDER BY cluster_id",
            )?;
            let rows = statement
                .query_map(params![self.table, field, term], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            posting_cursor_from_rows(rows)
        })?)
    }

    fn posting_cursors_bulk(
        &self,
        field: &str,
        terms: &[String],
    ) -> StorageBackendResult<Vec<Box<dyn PostingCursor>>> {
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let unique_terms = terms.iter().cloned().collect::<BTreeSet<_>>();
        let cursors = self.conn.with(|connection| {
            let mut by_term = BTreeMap::<String, Vec<(i64, i64, Vec<u8>)>>::new();
            let unique_terms = unique_terms.into_iter().collect::<Vec<_>>();
            for chunk in unique_terms.chunks(900) {
                let placeholders = std::iter::repeat_n("?", chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "SELECT term, cluster_id, posting_count, score_blob FROM _posting_clusters
                      WHERE table_name = ? AND field = ? AND term IN ({placeholders})
                      ORDER BY term, cluster_id"
                );
                let mut values = Vec::with_capacity(chunk.len() + 2);
                values.push(SqlValue::Text(self.table.clone()));
                values.push(SqlValue::Text(field.to_string()));
                values.extend(chunk.iter().cloned().map(SqlValue::Text));
                let mut statement = connection.prepare(&sql)?;
                let rows = statement.query_map(params_from_iter(values), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                })?;
                for row in rows {
                    let (term, cluster_id, posting_count, score_blob) = row?;
                    by_term
                        .entry(term)
                        .or_default()
                        .push((cluster_id, posting_count, score_blob));
                }
            }
            let mut cursors = BTreeMap::new();
            for term in unique_terms {
                cursors.insert(
                    term.clone(),
                    posting_cursor_from_rows(by_term.remove(&term).unwrap_or_default())?,
                );
            }
            Ok(cursors)
        })?;
        Ok(terms.iter().map(|term| cursors[term].clone()).collect())
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
        let terms = self.terms_for_field(field)?;
        self.ensure_aux_tables(field)?;
        let table = self.blockmax_table_name(field);
        self.conn.with_mut(|conn| {
            conn.execute(&format!("DELETE FROM {}", quote_ident(&table)), [])?;
            Ok(())
        })?;
        for term in terms {
            self.build_block_max_scores_versioned(field, &term, scorer, scorer_fingerprint)?;
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
        Ok(self.conn.with(|c| {
            let n: i64 = c.query_row(
                "SELECT COALESCE(SUM(posting_count), 0) FROM _posting_clusters
                     WHERE table_name = ?1 AND field = ?2 AND term = ?3",
                params![self.table, field, term],
                |r| r.get(0),
            )?;
            decode_index_u64("document frequency", n)
        })?)
    }

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> StorageBackendResult<u64> {
        let doc_id = encode_index_u64("document", doc_id)?;
        Ok(self.conn.with(|c| {
            let n: Option<i64> = c
                .query_row(
                    "SELECT length FROM _doc_lengths
                         WHERE table_name = ?1 AND doc_id = ?2 AND field = ?3",
                    params![self.table, doc_id, field],
                    |r| r.get(0),
                )
                .optional()?;
            n.map_or(Ok(0), |length| decode_index_u64("document length", length))
        })?)
    }

    fn get_doc_lengths_bulk(
        &self,
        doc_ids: &[DocId],
        field: &str,
    ) -> StorageBackendResult<BTreeMap<DocId, u64>> {
        if doc_ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        Ok(self.conn.with(|c| {
            let mut out = BTreeMap::new();
            for chunk in doc_ids.chunks(900) {
                let placeholders = std::iter::repeat_n("?", chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "SELECT doc_id, length FROM _doc_lengths
                         WHERE table_name = ? AND field = ? AND doc_id IN ({placeholders})"
                );
                let mut values = Vec::with_capacity(chunk.len() + 2);
                values.push(SqlValue::Text(self.table.clone()));
                values.push(SqlValue::Text(field.to_string()));
                for doc_id in chunk {
                    values.push(SqlValue::Integer(encode_index_u64("document", *doc_id)?));
                }
                let mut stmt = c.prepare(&sql)?;
                let rows = stmt.query_map(params_from_iter(values), |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })?;
                for row in rows {
                    let (doc_id, length) = row?;
                    let doc_id = decode_index_u64("document id", doc_id)?;
                    let length = decode_index_u64("document length", length)?;
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
            .posting_cursors_bulk(field, terms)?
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
        let posting_cluster = encode_index_u64("posting cluster", cluster_id(doc_id))?;
        Ok(self.conn.with(|c| {
            let blob: Option<Vec<u8>> = c
                .query_row(
                    "SELECT score_blob FROM _posting_clusters
                         WHERE table_name = ?1 AND field = ?2
                            AND term = ?3 AND cluster_id = ?4",
                    params![self.table, field, term, posting_cluster],
                    |r| r.get(0),
                )
                .optional()?;
            match blob {
                Some(blob) => {
                    let scores = clustered_result(decode_all_scores(cluster_id(doc_id), &blob))?;
                    Ok(scores
                        .binary_search_by_key(&doc_id, |entry| entry.doc_id)
                        .ok()
                        .map_or(0, |position| scores[position].term_freq))
                }
                None => Ok(0),
            }
        })?)
    }

    fn doc_count(&self) -> StorageBackendResult<u64> {
        Ok(self.conn.with(|c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(DISTINCT doc_id) FROM _doc_lengths
                     WHERE table_name = ?1",
                params![self.table],
                |r| r.get(0),
            )?;
            decode_index_u64("document count", n)
        })?)
    }

    fn total_field_length(&self, field: &str) -> StorageBackendResult<u64> {
        Ok(self.conn.with(|c| {
            let n: Option<i64> = c
                .query_row(
                    "SELECT total_length FROM _field_stats
                         WHERE table_name = ?1 AND field = ?2",
                    params![self.table, field],
                    |r| r.get(0),
                )
                .optional()?;
            n.map_or(Ok(0), |length| {
                decode_index_u64("total field length", length)
            })
        })?)
    }

    fn vocabulary_terms(&self, field: &str) -> StorageBackendResult<Vec<String>> {
        self.terms_for_field(field)
    }

    fn stats(&self) -> StorageBackendResult<IndexStats> {
        let doc_count = self.doc_count()?;
        let mut s = IndexStats::default();
        s.total_docs = doc_count;
        if doc_count > 0 {
            let total: u64 = self.conn.with(|c| {
                let n: i64 = c.query_row(
                    "SELECT COALESCE(SUM(total_length), 0) FROM _field_stats
                         WHERE table_name = ?1",
                    params![self.table],
                    |r| r.get(0),
                )?;
                decode_index_u64("total indexed length", n)
            })?;
            s.avg_doc_length = total as f64 / doc_count as f64;
        }
        // Pull all (field, term) doc-frequencies in one query.
        let pairs: Vec<(String, String, u64)> = self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT field, term, SUM(posting_count) FROM _posting_clusters
                     WHERE table_name = ?1
                     GROUP BY field, term",
            )?;
            let rows = stmt.query_map(params![self.table], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (field, term, doc_frequency) = row?;
                out.push((
                    field,
                    term,
                    decode_index_u64("document frequency", doc_frequency)?,
                ));
            }
            Ok(out)
        })?;
        for (field, term, df) in pairs {
            s.set_doc_freq(field, term, df);
        }
        Ok(s)
    }

    fn posting_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        Ok(self.conn.with(|c| {
            let n: i64 = if let Some(field) = field {
                c.query_row(
                    "SELECT COALESCE(SUM(posting_count), 0) FROM _posting_clusters
                         WHERE table_name = ?1 AND field = ?2",
                    params![self.table, field],
                    |r| r.get(0),
                )?
            } else {
                c.query_row(
                    "SELECT COALESCE(SUM(posting_count), 0) FROM _posting_clusters
                     WHERE table_name = ?1",
                    params![self.table],
                    |r| r.get(0),
                )?
            };
            decode_index_u64("posting count", n)
        })?)
    }

    fn doc_length_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        Ok(self.conn.with(|c| {
            let n: i64 = if let Some(field) = field {
                c.query_row(
                    "SELECT COUNT(*) FROM _doc_lengths
                         WHERE table_name = ?1 AND field = ?2",
                    params![self.table, field],
                    |r| r.get(0),
                )?
            } else {
                c.query_row(
                    "SELECT COUNT(*) FROM _doc_lengths WHERE table_name = ?1",
                    params![self.table],
                    |r| r.get(0),
                )?
            };
            decode_index_u64("document length count", n)
        })?)
    }

    fn term_count(&self, field: Option<&str>) -> StorageBackendResult<u64> {
        Ok(self.conn.with(|c| {
            let n: i64 = if let Some(field) = field {
                c.query_row(
                    "SELECT COUNT(DISTINCT term) FROM _posting_clusters
                         WHERE table_name = ?1 AND field = ?2",
                    params![self.table, field],
                    |r| r.get(0),
                )?
            } else {
                c.query_row(
                    "SELECT COUNT(DISTINCT term) FROM _posting_clusters WHERE table_name = ?1",
                    params![self.table],
                    |r| r.get(0),
                )?
            };
            decode_index_u64("term count", n)
        })?)
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn InvertedIndex>> {
        Ok(Arc::new(self.clone()))
    }

    fn field_names(&self) -> StorageBackendResult<Vec<FieldName>> {
        Ok(self.conn.with(|c| {
            let mut stmt =
                c.prepare("SELECT DISTINCT field FROM _doc_lengths WHERE table_name = ?1")?;
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
