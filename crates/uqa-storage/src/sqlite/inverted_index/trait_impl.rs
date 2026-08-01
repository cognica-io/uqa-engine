//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `InvertedIndex` trait implementation and read/statistics surface.

use super::{
    blob_to_positions, decode_index_u64, encode_index_u64, invalidate_block_max_tables, params,
    params_from_iter, quote_ident, usize_to_index_u64, Analyzer, AnalyzerPhase, Arc, BTreeMap,
    BTreeSet, BlockMaxScorer, DocId, FieldName, IndexStats, InvertedIndex, OptionalExtension,
    Payload, PostingEntry, PostingList, SQLiteError, SQLiteInvertedIndex, SqlValue,
    StorageBackendResult,
};

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
                "DELETE FROM _postings WHERE table_name = ?1",
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
                "SELECT doc_id, positions FROM _postings
                     WHERE table_name = ?1 AND field = ?2 AND term = ?3
                     ORDER BY doc_id",
            )?;
            let rows = stmt.query_map(params![self.table, field, term], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?;
            let mut entries = Vec::new();
            for row in rows {
                let (doc_id, blob) = row?;
                let positions = blob_to_positions(&blob)?;
                entries.push(PostingEntry::new(
                    decode_index_u64("document id", doc_id)?,
                    Payload {
                        positions,
                        score: 0.0,
                        fields: BTreeMap::new(),
                    },
                ));
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
                    "SELECT term, doc_id, positions FROM _postings
                         WHERE table_name = ? AND field = ? AND term IN ({placeholders})
                         ORDER BY term, doc_id"
                );
                let mut values = Vec::with_capacity(chunk.len() + 2);
                values.push(SqlValue::Text(self.table.clone()));
                values.push(SqlValue::Text(field.to_string()));
                values.extend(chunk.iter().cloned().map(SqlValue::Text));
                let mut stmt = c.prepare(&sql)?;
                let rows = stmt.query_map(params_from_iter(values), |r| {
                    let term = r.get::<_, String>(0)?;
                    let doc_id = r.get::<_, i64>(1)?;
                    let blob = r.get::<_, Vec<u8>>(2)?;
                    Ok((term, doc_id, blob))
                })?;
                for row in rows {
                    let (term, doc_id, blob) = row?;
                    let entry = PostingEntry::new(
                        decode_index_u64("document id", doc_id)?,
                        Payload {
                            positions: blob_to_positions(&blob)?,
                            score: 0.0,
                            fields: BTreeMap::new(),
                        },
                    );
                    by_term.entry(term).or_default().push(entry);
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
        self.conn.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT doc_id, positions FROM _postings
                 WHERE table_name = ?1 AND field = ?2 AND term = ?3
                 ORDER BY doc_id",
            )?;
            let mut rows = stmt.query(params![self.table, field, term])?;
            while let Some(row) = rows.next()? {
                let doc_id = decode_index_u64("document id", row.get::<_, i64>(0)?)?;
                let positions = blob_to_positions(&row.get::<_, Vec<u8>>(1)?)?;
                visit(
                    doc_id,
                    usize_to_index_u64("term frequency", positions.len())?,
                );
            }
            Ok(())
        })?;
        Ok(())
    }

    fn doc_freq(&self, field: &str, term: &str) -> StorageBackendResult<u64> {
        Ok(self.conn.with(|c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM _postings
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
        self.conn.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT doc_id, positions
                 FROM _postings
                 WHERE table_name = ?1 AND field = ?2 AND term = ?3
                 ORDER BY doc_id",
            )?;
            for (term_index, term) in terms.iter().enumerate() {
                let mut rows = stmt.query(params![self.table, field, term])?;
                while let Some(row) = rows.next()? {
                    let doc_id = decode_index_u64("document id", row.get::<_, i64>(0)?)?;
                    let term_freq = usize_to_index_u64(
                        "term frequency",
                        blob_to_positions(&row.get::<_, Vec<u8>>(1)?)?.len(),
                    )?;
                    if let Some(positions) = output_positions.get(&doc_id) {
                        for position in positions {
                            inputs[*position].1[term_index] = term_freq;
                        }
                    }
                }
            }
            Ok(())
        })?;
        Ok(inputs)
    }

    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> StorageBackendResult<u64> {
        let doc_id = encode_index_u64("document", doc_id)?;
        Ok(self.conn.with(|c| {
            let blob: Option<Vec<u8>> = c
                .query_row(
                    "SELECT positions FROM _postings
                         WHERE table_name = ?1 AND field = ?2
                            AND term = ?3 AND doc_id = ?4",
                    params![self.table, field, term, doc_id],
                    |r| r.get(0),
                )
                .optional()?;
            match blob {
                Some(blob) => usize_to_index_u64("term frequency", blob_to_positions(&blob)?.len()),
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
                "SELECT field, term, COUNT(*) FROM _postings
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
                    "SELECT COUNT(*) FROM _postings
                         WHERE table_name = ?1 AND field = ?2",
                    params![self.table, field],
                    |r| r.get(0),
                )?
            } else {
                c.query_row(
                    "SELECT COUNT(*) FROM _postings WHERE table_name = ?1",
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
                    "SELECT COUNT(DISTINCT term) FROM _postings
                         WHERE table_name = ?1 AND field = ?2",
                    params![self.table, field],
                    |r| r.get(0),
                )?
            } else {
                c.query_row(
                    "SELECT COUNT(DISTINCT term) FROM _postings WHERE table_name = ?1",
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
