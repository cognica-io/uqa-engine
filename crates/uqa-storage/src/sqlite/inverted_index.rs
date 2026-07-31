//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite`-backed [`InvertedIndex`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, OptionalExtension};
use uqa_analysis::Analyzer;
use uqa_core::{DocId, FieldName, IndexStats, Payload, PostingEntry, PostingList};

use crate::block_max_index::{BlockMaxIndex, BlockMaxScorer, DEFAULT_BLOCK_SIZE};
use crate::inverted_index::{AnalyzerPhase, InvertedIndex};
use crate::sqlite::connection::{ManagedConnection, Result as SQLiteResult, SQLiteError};
use crate::StorageBackendResult;

#[derive(Clone)]
pub struct SQLiteInvertedIndex {
    conn: ManagedConnection,
    table: String,
    analyzer: Analyzer,
    /// Per-field index-time analyzer overrides. Persisted at engine
    /// catalog layer (not on the index itself); the index just looks
    /// them up at tokenization time.
    index_field_analyzers: BTreeMap<FieldName, Analyzer>,
    /// Per-field search-time analyzer overrides.
    search_field_analyzers: BTreeMap<FieldName, Analyzer>,
}

#[derive(Debug)]
struct StagedField {
    length: u64,
    postings: Vec<(String, Vec<u8>)>,
}

impl SQLiteInvertedIndex {
    pub const BLOCK_SIZE: usize = DEFAULT_BLOCK_SIZE;

    pub fn new(conn: ManagedConnection, table: impl Into<String>, analyzer: Analyzer) -> Self {
        Self {
            conn,
            table: table.into(),
            analyzer,
            index_field_analyzers: BTreeMap::new(),
            search_field_analyzers: BTreeMap::new(),
        }
    }

    /// Tokenize `text` against the analyzer bound to `field`. Mirrors
    /// the canonical UQA implementation's `SQLiteInvertedIndex._tokenize`.
    pub fn tokenize(&self, text: &str, field: &str) -> StorageBackendResult<Vec<String>> {
        let analyzer = self
            .index_field_analyzers
            .get(field)
            .unwrap_or(&self.analyzer);
        Ok(analyzer.analyze(text)?)
    }

    pub fn skip_table_name(&self, field: &str) -> String {
        format!("_skip_{}_{}", self.table, field)
    }

    pub fn blockmax_table_name(&self, field: &str) -> String {
        format!("_blockmax_{}_{}", self.table, field)
    }

    pub fn flush_skip_pointers(&self) -> StorageBackendResult<()> {
        let fields = self.field_names()?;
        for field in fields {
            self.rebuild_skip_pointers_for_field(&field)?;
        }
        Ok(())
    }

    /// Read the nearest materialized skip pointer without mutating storage.
    /// Call [`Self::flush_skip_pointers`] from an explicit maintenance/write
    /// boundary after postings change.
    pub fn skip_to(
        &self,
        field: &str,
        term: &str,
        target_doc_id: DocId,
    ) -> StorageBackendResult<(DocId, usize)> {
        let table = self.skip_table_name(field);
        let target_doc_id = encode_index_u64("target document", target_doc_id)?;
        Ok(self.conn.with(|conn| {
            if !table_exists(conn, &table)? {
                return Ok((0, 0));
            }
            let sql = format!(
                "SELECT skip_doc_id, skip_offset FROM {}
                     WHERE term = ?1 AND skip_doc_id <= ?2
                     ORDER BY skip_doc_id DESC LIMIT 1",
                quote_ident(&table)
            );
            let row: Option<(i64, i64)> = conn
                .query_row(&sql, params![term, target_doc_id], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()?;
            match row {
                Some((doc_id, offset)) => Ok((
                    decode_index_u64("skip document", doc_id)?,
                    decode_index_usize("skip offset", offset)?,
                )),
                None => Ok((0, 0)),
            }
        })?)
    }

    pub fn build_block_max_scores<S: BlockMaxScorer + ?Sized>(
        &self,
        field: &str,
        term: &str,
        scorer: &S,
    ) -> StorageBackendResult<()> {
        self.build_block_max_scores_versioned(field, term, scorer, "")
    }

    /// Build one scorer-versioned block-max posting. The version is checked
    /// on load so changed BM25 parameters or field statistics can never feed
    /// an unsafe pruning bound.
    pub fn build_block_max_scores_versioned<S: BlockMaxScorer + ?Sized>(
        &self,
        field: &str,
        term: &str,
        scorer: &S,
        scorer_fingerprint: &str,
    ) -> StorageBackendResult<()> {
        let posting_list = self.get_posting_list(field, term)?;
        if posting_list.is_empty() && !self.has_field(field)? {
            return Ok(());
        }
        let mut scored_entries = Vec::with_capacity(posting_list.len());
        for entry in posting_list.entries() {
            let tf = usize_to_index_u64("term frequency", entry.payload.positions.len().max(1))?;
            let doc_length = self.get_doc_length(entry.doc_id, field)?.max(tf);
            scored_entries.push((tf, doc_length));
        }
        self.ensure_aux_tables(field)?;
        let table = self.blockmax_table_name(field);
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                &format!("DELETE FROM {} WHERE term = ?1", quote_ident(&table)),
                [term],
            )?;
            let df = usize_to_index_u64("document frequency", posting_list.len())?;
            for (block_idx, chunk) in scored_entries.chunks(Self::BLOCK_SIZE).enumerate() {
                let mut max_score = 0.0_f64;
                for &(tf, doc_length) in chunk {
                    let score = scorer.score(tf, doc_length, df);
                    if !score.is_finite() || score < 0.0 {
                        return Err(SQLiteError::StorageBackend(format!(
                            "block-max score must be finite and non-negative, got {score}"
                        )));
                    }
                    max_score = max_score.max(score);
                }
                let block_idx = encode_index_usize("block index", block_idx)?;
                tx.execute(
                    &format!(
                        "INSERT OR REPLACE INTO {}
                            (term, block_idx, max_score, scorer_fingerprint)
                         VALUES (?1, ?2, ?3, ?4)",
                        quote_ident(&table)
                    ),
                    params![term, block_idx, max_score, scorer_fingerprint],
                )?;
            }
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn build_all_block_max_scores<S: BlockMaxScorer + ?Sized>(
        &self,
        field: &str,
        scorer: &S,
    ) -> StorageBackendResult<()> {
        let terms = self.terms_for_field(field)?;
        for term in terms {
            self.build_block_max_scores(field, &term, scorer)?;
        }
        Ok(())
    }

    pub fn get_block_max_score(
        &self,
        field: &str,
        term: &str,
        block_idx: usize,
    ) -> StorageBackendResult<f64> {
        let table = self.blockmax_table_name(field);
        let block_idx = encode_index_usize("block index", block_idx)?;
        Ok(self.conn.with(|conn| {
            if !table_exists(conn, &table)? {
                return Ok(0.0);
            }
            let sql = format!(
                "SELECT max_score FROM {}
                     WHERE term = ?1 AND block_idx = ?2",
                quote_ident(&table)
            );
            let score: Option<f64> = conn
                .query_row(&sql, params![term, block_idx], |row| row.get(0))
                .optional()?;
            Ok(score.unwrap_or(0.0))
        })?)
    }

    pub fn get_all_block_max_scores(
        &self,
        field: &str,
        term: &str,
    ) -> StorageBackendResult<Vec<f64>> {
        let table = self.blockmax_table_name(field);
        Ok(self.conn.with(|conn| {
            if !table_exists(conn, &table)? {
                return Ok(Vec::new());
            }
            let sql = format!(
                "SELECT block_idx, max_score FROM {}
                     WHERE term = ?1 ORDER BY block_idx",
                quote_ident(&table)
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map([term], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut scores = Vec::with_capacity(rows.len());
            for (expected, (block_idx, score)) in rows.into_iter().enumerate() {
                let block_idx = decode_index_usize("block index", block_idx)?;
                if block_idx != expected {
                    return Err(SQLiteError::StorageBackend(format!(
                        "corrupt inverted index: expected block index {expected}, found {block_idx}"
                    )));
                }
                scores.push(score);
            }
            Ok(scores)
        })?)
    }

    pub fn get_versioned_block_max_scores(
        &self,
        field: &str,
        term: &str,
        scorer_fingerprint: &str,
    ) -> StorageBackendResult<Option<Vec<f64>>> {
        let table = self.blockmax_table_name(field);
        Ok(self.conn.with(|conn| {
            if !table_exists(conn, &table)? {
                return Ok(None);
            }
            let pragma = format!("PRAGMA table_info({})", quote_ident(&table));
            let mut columns = conn.prepare(&pragma)?;
            let has_fingerprint = columns
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .any(|name| name == "scorer_fingerprint");
            drop(columns);
            if !has_fingerprint {
                return Ok(None);
            }
            let sql = format!(
                "SELECT block_idx, max_score FROM {}
                 WHERE term = ?1 AND scorer_fingerprint = ?2
                 ORDER BY block_idx",
                quote_ident(&table)
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params![term, scorer_fingerprint], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            if rows.is_empty() {
                return Ok(None);
            }
            let mut scores = Vec::with_capacity(rows.len());
            for (expected, (block_idx, score)) in rows.into_iter().enumerate() {
                let block_idx = decode_index_usize("block index", block_idx)?;
                if block_idx != expected || !score.is_finite() || score < 0.0 {
                    return Err(SQLiteError::StorageBackend(format!(
                        "corrupt block-max index for `{field}.{term}` at block {block_idx}"
                    )));
                }
                scores.push(score);
            }
            Ok(Some(scores))
        })?)
    }

    pub fn load_block_max_into(&self, target: &mut BlockMaxIndex) -> StorageBackendResult<()> {
        for field in self.fields_with_blockmax_tables()? {
            for term in self.terms_for_field(&field)? {
                let scores = self.get_all_block_max_scores(&field, &term)?;
                if !scores.is_empty() {
                    target.set_block_maxes(&self.table, &field, &term, scores)?;
                }
            }
        }
        Ok(())
    }

    fn has_field(&self, field: &str) -> SQLiteResult<bool> {
        self.conn.with(|conn| {
            let found: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM _doc_lengths
                     WHERE table_name = ?1 AND field = ?2 LIMIT 1",
                    params![self.table, field],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(found.is_some())
        })
    }

    fn terms_for_field(&self, field: &str) -> StorageBackendResult<Vec<String>> {
        Ok(self.conn.with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT term FROM _postings
                     WHERE table_name = ?1 AND field = ?2
                     ORDER BY term",
            )?;
            let rows = stmt
                .query_map(params![self.table, field], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?)
    }

    fn fields_with_blockmax_tables(&self) -> StorageBackendResult<Vec<String>> {
        let prefix = format!("_blockmax_{}_", self.table);
        Ok(self.conn.with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT name FROM sqlite_master
                     WHERE type = 'table' AND name LIKE ?1
                     ORDER BY name",
            )?;
            let rows = stmt.query_map([format!("{prefix}%")], |row| row.get::<_, String>(0))?;
            let mut fields = Vec::new();
            for row in rows {
                let name = row?;
                if let Some(field) = name.strip_prefix(&prefix) {
                    fields.push(field.to_string());
                }
            }
            Ok(fields)
        })?)
    }

    fn ensure_aux_tables(&self, field: &str) -> SQLiteResult<()> {
        let skip_table = self.skip_table_name(field);
        let block_table = self.blockmax_table_name(field);
        self.conn
            .with(|conn| Self::ensure_aux_tables_on(conn, &skip_table, &block_table))
    }

    fn ensure_aux_tables_on(
        conn: &rusqlite::Connection,
        skip_table: &str,
        block_table: &str,
    ) -> SQLiteResult<()> {
        conn.execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS {} (
                    term TEXT NOT NULL,
                    skip_doc_id INTEGER NOT NULL,
                    skip_offset INTEGER NOT NULL,
                    PRIMARY KEY (term, skip_doc_id)
                )",
                quote_ident(skip_table)
            ),
            [],
        )?;
        conn.execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS {} (
                    term TEXT NOT NULL,
                    block_idx INTEGER NOT NULL,
                    max_score REAL NOT NULL,
                    scorer_fingerprint TEXT NOT NULL DEFAULT '',
                    PRIMARY KEY (term, block_idx)
                )",
                quote_ident(block_table)
            ),
            [],
        )?;
        let pragma = format!("PRAGMA table_info({})", quote_ident(block_table));
        let mut stmt = conn.prepare(&pragma)?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        if !columns.iter().any(|name| name == "scorer_fingerprint") {
            conn.execute(
                &format!(
                    "ALTER TABLE {} ADD COLUMN scorer_fingerprint TEXT NOT NULL DEFAULT ''",
                    quote_ident(block_table)
                ),
                [],
            )?;
        }
        Ok(())
    }

    fn analyze_fields(
        &self,
        fields: BTreeMap<FieldName, String>,
    ) -> SQLiteResult<BTreeMap<FieldName, StagedField>> {
        let mut staged = BTreeMap::new();
        for (field, text) in fields {
            let analyzer = self
                .index_field_analyzers
                .get(&field)
                .unwrap_or(&self.analyzer);
            let tokens = analyzer.analyze(&text)?;
            let length = usize_to_index_u64("document length", tokens.len())?;
            validate_position_count(length)?;
            let mut term_positions: BTreeMap<String, Vec<u32>> = BTreeMap::new();
            for (position, token) in tokens.into_iter().enumerate() {
                term_positions
                    .entry(token)
                    .or_default()
                    .push(u32::try_from(position).map_err(|_| {
                        SQLiteError::StorageBackend(
                            "token position exceeds the u32 index format".into(),
                        )
                    })?);
            }
            let mut postings = Vec::with_capacity(term_positions.len());
            for (term, mut positions) in term_positions {
                positions.sort_unstable();
                positions.dedup();
                postings.push((term, positions_to_blob(&positions)?));
            }
            staged.insert(field, StagedField { length, postings });
        }
        Ok(staged)
    }

    fn rebuild_skip_pointers_for_field(&self, field: &str) -> SQLiteResult<()> {
        if !self.has_field(field)? {
            return Ok(());
        }
        self.ensure_aux_tables(field)?;
        let table = self.skip_table_name(field);
        let persisted_postings: Vec<(String, i64)> = self.conn.with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT term, doc_id FROM _postings
                 WHERE table_name = ?1 AND field = ?2
                 ORDER BY term, doc_id",
            )?;
            let rows = stmt
                .query_map(params![self.table, field], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        let mut by_term: BTreeMap<String, Vec<DocId>> = BTreeMap::new();
        for (term, doc_id) in persisted_postings {
            let doc_id = decode_index_u64("document id", doc_id)?;
            by_term.entry(term).or_default().push(doc_id);
        }
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(&format!("DELETE FROM {}", quote_ident(&table)), [])?;
            for (term, docs) in by_term {
                for (block_idx, chunk) in docs.chunks(Self::BLOCK_SIZE).enumerate() {
                    if let Some(doc_id) = chunk.first() {
                        let doc_id = encode_index_u64("skip document", *doc_id)?;
                        let skip_offset = encode_index_usize(
                            "skip offset",
                            block_idx.checked_mul(Self::BLOCK_SIZE).ok_or_else(|| {
                                SQLiteError::StorageBackend(
                                    "skip-pointer offset overflow".to_string(),
                                )
                            })?,
                        )?;
                        tx.execute(
                            &format!(
                                "INSERT OR REPLACE INTO {}
                                    (term, skip_doc_id, skip_offset)
                                 VALUES (?1, ?2, ?3)",
                                quote_ident(&table)
                            ),
                            params![term, doc_id, skip_offset],
                        )?;
                    }
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    fn add_document_inner(
        &self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> SQLiteResult<()> {
        let doc_id = encode_index_u64("document", doc_id)?;
        let staged = self.analyze_fields(fields)?;
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            let old_lengths = load_document_lengths(&tx, &self.table, doc_id)?;
            let mut affected_fields = BTreeSet::new();
            affected_fields.extend(old_lengths.keys().cloned());
            affected_fields.extend(staged.keys().cloned());
            let mut planned_totals = Vec::with_capacity(affected_fields.len());
            for field in affected_fields {
                let current = load_field_total(&tx, &self.table, &field)?.unwrap_or(0);
                let old = old_lengths.get(&field).copied().unwrap_or(0);
                let new = staged.get(&field).map_or(0, |value| value.length);
                let total = current
                    .checked_sub(old)
                    .ok_or_else(|| corrupt_counter("total field length underflow"))?
                    .checked_add(new)
                    .ok_or_else(|| corrupt_counter("total field length overflow"))?;
                let total = encode_index_counter("total field length", total)?;
                let other_docs: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM _doc_lengths
                     WHERE table_name = ?1 AND field = ?2 AND doc_id <> ?3",
                    params![self.table, field, doc_id],
                    |row| row.get(0),
                )?;
                let has_field_after = decode_index_u64("field document count", other_docs)? > 0
                    || staged.contains_key(&field);
                planned_totals.push((field, total, has_field_after));
            }

            // No data mutation occurs until every analyzer, conversion, and
            // counter transition has been validated.
            for field in staged.keys() {
                Self::ensure_aux_tables_on(
                    &tx,
                    &self.skip_table_name(field),
                    &self.blockmax_table_name(field),
                )?;
            }
            invalidate_block_max_tables(&tx, &self.table)?;
            tx.execute(
                "DELETE FROM _postings WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, doc_id],
            )?;
            tx.execute(
                "DELETE FROM _doc_lengths WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, doc_id],
            )?;
            for (field, total, has_field_after) in planned_totals {
                if has_field_after {
                    tx.execute(
                        "INSERT INTO _field_stats (table_name, field, total_length)
                         VALUES (?1, ?2, ?3)
                         ON CONFLICT(table_name, field) DO UPDATE
                            SET total_length = excluded.total_length",
                        params![self.table, field, total],
                    )?;
                } else {
                    tx.execute(
                        "DELETE FROM _field_stats WHERE table_name = ?1 AND field = ?2",
                        params![self.table, field],
                    )?;
                }
            }

            for (field, staged_field) in staged {
                let length = encode_index_counter("document length", staged_field.length)?;
                tx.execute(
                    "INSERT OR REPLACE INTO _doc_lengths
                        (table_name, doc_id, field, length)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![self.table, doc_id, field, length],
                )?;
                for (term, blob) in staged_field.postings {
                    tx.execute(
                        "INSERT OR REPLACE INTO _postings
                            (table_name, field, term, doc_id, positions)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![self.table, field, term, doc_id, blob],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    fn rebuild_documents_inner(
        &self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> SQLiteResult<()> {
        let mut staged_documents = BTreeMap::new();
        for (doc_id, fields) in documents {
            if !fields.is_empty() {
                staged_documents.insert(
                    encode_index_u64("document", doc_id)?,
                    self.analyze_fields(fields)?,
                );
            }
        }
        let fields = staged_documents
            .values()
            .flat_map(|fields| fields.keys().cloned())
            .collect::<BTreeSet<_>>();
        let mut field_totals = BTreeMap::<FieldName, u64>::new();
        for staged_fields in staged_documents.values() {
            for (field, staged_field) in staged_fields {
                let total = field_totals.entry(field.clone()).or_default();
                *total = total
                    .checked_add(staged_field.length)
                    .ok_or_else(|| corrupt_counter("total field length overflow"))?;
            }
        }

        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            for field in &fields {
                Self::ensure_aux_tables_on(
                    &tx,
                    &self.skip_table_name(field),
                    &self.blockmax_table_name(field),
                )?;
            }
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

            {
                let mut insert_length = tx.prepare(
                    "INSERT OR REPLACE INTO _doc_lengths
                        (table_name, doc_id, field, length)
                     VALUES (?1, ?2, ?3, ?4)",
                )?;
                let mut insert_posting = tx.prepare(
                    "INSERT OR REPLACE INTO _postings
                        (table_name, field, term, doc_id, positions)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )?;

                for (doc_id, fields) in staged_documents {
                    for (field, staged_field) in fields {
                        let length = encode_index_counter("document length", staged_field.length)?;
                        insert_length.execute(params![self.table, doc_id, field, length])?;
                        for (term, blob) in staged_field.postings {
                            insert_posting
                                .execute(params![self.table, field, term, doc_id, blob])?;
                        }
                    }
                }
            }

            {
                let mut insert_stats = tx.prepare(
                    "INSERT INTO _field_stats (table_name, field, total_length)
                     VALUES (?1, ?2, ?3)",
                )?;
                for (field, total_length) in field_totals {
                    insert_stats.execute(params![
                        self.table,
                        field,
                        encode_index_counter("total field length", total_length)?
                    ])?;
                }
            }

            tx.commit()?;
            Ok(())
        })
    }

    fn remove_document_inner(&self, doc_id: DocId) -> SQLiteResult<()> {
        let doc_id = encode_index_u64("document", doc_id)?;
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            let old_lengths = load_document_lengths(&tx, &self.table, doc_id)?;
            let mut planned_totals = Vec::with_capacity(old_lengths.len());
            for (field, length) in &old_lengths {
                let current = load_field_total(&tx, &self.table, field)?.unwrap_or(0);
                let total = current
                    .checked_sub(*length)
                    .ok_or_else(|| corrupt_counter("total field length underflow"))?;
                let other_docs: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM _doc_lengths
                     WHERE table_name = ?1 AND field = ?2 AND doc_id <> ?3",
                    params![self.table, field, doc_id],
                    |row| row.get(0),
                )?;
                planned_totals.push((
                    field.clone(),
                    encode_index_counter("total field length", total)?,
                    decode_index_u64("field document count", other_docs)? > 0,
                ));
            }
            invalidate_block_max_tables(&tx, &self.table)?;
            tx.execute(
                "DELETE FROM _doc_lengths WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, doc_id],
            )?;
            tx.execute(
                "DELETE FROM _postings WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, doc_id],
            )?;
            for (field, total, has_field_after) in planned_totals {
                if has_field_after {
                    tx.execute(
                        "UPDATE _field_stats SET total_length = ?3
                         WHERE table_name = ?1 AND field = ?2",
                        params![self.table, field, total],
                    )?;
                } else {
                    tx.execute(
                        "DELETE FROM _field_stats WHERE table_name = ?1 AND field = ?2",
                        params![self.table, field],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }
}

/// A score materialization is valid only for the exact posting/statistics
/// snapshot it was built from. Clear every field-local block-max table in the
/// same transaction as a posting mutation; stale bounds could otherwise make
/// an exact top-k query return the wrong documents.
fn invalidate_block_max_tables(
    conn: &rusqlite::Connection,
    logical_table: &str,
) -> SQLiteResult<()> {
    let prefix = format!("_blockmax_{logical_table}_");
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for name in names.into_iter().filter(|name| name.starts_with(&prefix)) {
        conn.execute(&format!("DELETE FROM {}", quote_ident(&name)), [])?;
    }
    Ok(())
}

fn positions_to_blob(positions: &[u32]) -> SQLiteResult<Vec<u8>> {
    let capacity = positions
        .len()
        .checked_mul(std::mem::size_of::<u32>())
        .ok_or_else(|| corrupt_counter("posting-position payload size overflow"))?;
    let mut buf = Vec::with_capacity(capacity);
    for p in positions {
        buf.extend_from_slice(&p.to_le_bytes());
    }
    Ok(buf)
}

fn blob_to_positions(blob: &[u8]) -> SQLiteResult<Vec<u32>> {
    if blob.len() % 4 != 0 {
        return Err(SQLiteError::StorageBackend(
            "invalid posting positions payload".to_string(),
        ));
    }
    Ok(blob
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn encode_index_u64(kind: &str, value: u64) -> SQLiteResult<i64> {
    i64::try_from(value).map_err(|_| {
        SQLiteError::StorageBackend(format!(
            "{kind} id {value} exceeds the SQLite INTEGER range"
        ))
    })
}

fn encode_index_usize(kind: &str, value: usize) -> SQLiteResult<i64> {
    i64::try_from(value).map_err(|_| {
        SQLiteError::StorageBackend(format!("{kind} {value} exceeds the SQLite INTEGER range"))
    })
}

fn usize_to_index_u64(kind: &str, value: usize) -> SQLiteResult<u64> {
    u64::try_from(value).map_err(|_| {
        SQLiteError::StorageBackend(format!("{kind} {value} exceeds the u64 counter range"))
    })
}

fn encode_index_counter(kind: &str, value: u64) -> SQLiteResult<i64> {
    i64::try_from(value).map_err(|_| {
        SQLiteError::StorageBackend(format!("{kind} {value} exceeds the SQLite INTEGER range"))
    })
}

fn validate_position_count(token_count: u64) -> SQLiteResult<()> {
    if token_count > u64::from(u32::MAX) + 1 {
        return Err(SQLiteError::StorageBackend(
            "token positions exceed the u32 index format".into(),
        ));
    }
    Ok(())
}

fn corrupt_counter(message: &str) -> SQLiteError {
    SQLiteError::StorageBackend(format!("corrupt inverted index: {message}"))
}

fn load_document_lengths(
    conn: &rusqlite::Connection,
    table: &str,
    doc_id: i64,
) -> SQLiteResult<BTreeMap<FieldName, u64>> {
    let mut stmt = conn.prepare(
        "SELECT field, length FROM _doc_lengths
         WHERE table_name = ?1 AND doc_id = ?2",
    )?;
    let rows = stmt.query_map(params![table, doc_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut lengths = BTreeMap::new();
    for row in rows {
        let (field, length) = row?;
        lengths.insert(field, decode_index_u64("document length", length)?);
    }
    Ok(lengths)
}

fn load_field_total(
    conn: &rusqlite::Connection,
    table: &str,
    field: &str,
) -> SQLiteResult<Option<u64>> {
    let total: Option<i64> = conn
        .query_row(
            "SELECT total_length FROM _field_stats
             WHERE table_name = ?1 AND field = ?2",
            params![table, field],
            |row| row.get(0),
        )
        .optional()?;
    total
        .map(|value| decode_index_u64("total field length", value))
        .transpose()
}

fn decode_index_u64(kind: &str, value: i64) -> SQLiteResult<u64> {
    u64::try_from(value).map_err(|_| {
        SQLiteError::StorageBackend(format!("corrupt inverted index: negative {kind} {value}"))
    })
}

fn decode_index_usize(kind: &str, value: i64) -> SQLiteResult<usize> {
    usize::try_from(value).map_err(|_| {
        SQLiteError::StorageBackend(format!("corrupt inverted index: invalid {kind} {value}"))
    })
}

fn table_exists(conn: &rusqlite::Connection, name: &str) -> rusqlite::Result<bool> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            [name],
            |row| row.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::catalog::Catalog;
    use uqa_analysis::{standard_analyzer, Analyzer, Tokenizer};

    fn fields<const N: usize>(pairs: [(&str, &str); N]) -> BTreeMap<FieldName, String> {
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn idx() -> SQLiteInvertedIndex {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _cat = Catalog::open(mc.clone()).unwrap();
        SQLiteInvertedIndex::new(mc, "articles", standard_analyzer("english"))
    }

    #[test]
    fn add_get_round_trip() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust language")]))
            .unwrap();
        idx.add_document(2, fields([("title", "python language")]))
            .unwrap();
        let pl = idx.get_posting_list("title", "languag").unwrap();
        let docs: Vec<_> = pl.doc_ids().collect();
        assert_eq!(docs, vec![1, 2]);
    }

    #[test]
    fn doc_freq_and_term_freq() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust rust rust")]))
            .unwrap();
        idx.add_document(2, fields([("title", "rust")])).unwrap();
        assert_eq!(idx.doc_freq("title", "rust").unwrap(), 2);
        assert_eq!(idx.get_term_freq(1, "title", "rust").unwrap(), 3);
        assert_eq!(idx.get_term_freq(2, "title", "rust").unwrap(), 1);
        let mut visited = Vec::new();
        idx.for_each_term_freq("title", "rust", &mut |doc_id, term_freq| {
            visited.push((doc_id, term_freq));
        })
        .unwrap();
        assert_eq!(visited, vec![(1, 3), (2, 1)]);
    }

    #[test]
    fn bulk_posting_lists_match_point_lookups() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust language")]))
            .unwrap();
        idx.add_document(2, fields([("title", "python language")]))
            .unwrap();
        idx.add_document(3, fields([("title", "rust search")]))
            .unwrap();

        let terms = vec![
            "rust".to_string(),
            "languag".to_string(),
            "missing".to_string(),
            "rust".to_string(),
        ];
        let bulk = idx.get_posting_lists_bulk("title", &terms).unwrap();
        assert_eq!(bulk.len(), terms.len());
        for (term, posting_list) in terms.iter().zip(&bulk) {
            assert_eq!(posting_list, &idx.get_posting_list("title", term).unwrap());
        }
    }

    #[test]
    fn bulk_doc_lengths_match_point_lookups() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust language")]))
            .unwrap();
        idx.add_document(2, fields([("title", "python")])).unwrap();
        idx.add_document(3, fields([("title", "sqlite search engine")]))
            .unwrap();

        let bulk = idx.get_doc_lengths_bulk(&[3, 1, 99, 2], "title").unwrap();
        assert_eq!(bulk.get(&1), Some(&idx.get_doc_length(1, "title").unwrap()));
        assert_eq!(bulk.get(&2), Some(&idx.get_doc_length(2, "title").unwrap()));
        assert_eq!(bulk.get(&3), Some(&idx.get_doc_length(3, "title").unwrap()));
        assert_eq!(bulk.get(&99), None);
    }

    #[test]
    fn bulk_scoring_inputs_match_point_lookups_in_requested_order() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust rust language")]))
            .unwrap();
        idx.add_document(2, fields([("title", "python language")]))
            .unwrap();
        idx.add_document(3, fields([("title", "rust search engine")]))
            .unwrap();

        let doc_ids = [3, 1, 99, 2, 1];
        let terms = ["rust", "languag", "missing"].map(str::to_string);
        let bulk = idx
            .get_scoring_inputs_bulk(&doc_ids, "title", &terms)
            .unwrap();
        let expected: Vec<_> = doc_ids
            .iter()
            .map(|doc_id| {
                (
                    idx.get_doc_length(*doc_id, "title").unwrap(),
                    terms
                        .iter()
                        .map(|term| idx.get_term_freq(*doc_id, "title", term).unwrap())
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(bulk, expected);
    }

    #[test]
    fn negative_persisted_document_ids_are_rejected_by_every_posting_reader() {
        let idx = idx();
        idx.conn
            .with(|connection| {
                connection.execute(
                    "INSERT INTO _postings
                        (table_name, field, term, doc_id, positions)
                     VALUES ('articles', 'title', 'rust', -1, ?1)",
                    [positions_to_blob(&[0]).unwrap()],
                )?;
                Ok(())
            })
            .unwrap();

        let point_error = idx.get_posting_list("title", "rust").unwrap_err();
        assert!(point_error.to_string().contains("negative document id -1"));

        let terms = vec!["rust".to_string()];
        let bulk_error = idx.get_posting_lists_bulk("title", &terms).unwrap_err();
        assert!(bulk_error.to_string().contains("negative document id -1"));

        let mut visited = Vec::new();
        let visit_error = idx
            .for_each_term_freq("title", "rust", &mut |doc_id, frequency| {
                visited.push((doc_id, frequency));
            })
            .unwrap_err();
        assert!(visit_error.to_string().contains("negative document id -1"));
        assert!(visited.is_empty());

        let scoring_error = idx
            .get_scoring_inputs_bulk(&[1], "title", &terms)
            .unwrap_err();
        assert!(scoring_error
            .to_string()
            .contains("negative document id -1"));
    }

    #[test]
    fn negative_persisted_lengths_and_block_indexes_are_rejected() {
        let idx = idx();
        idx.ensure_aux_tables("title").unwrap();
        let block_table = idx.blockmax_table_name("title");
        idx.conn
            .with(|connection| {
                connection.execute(
                    "INSERT INTO _doc_lengths (table_name, doc_id, field, length)
                     VALUES ('articles', 1, 'title', -2)",
                    [],
                )?;
                connection.execute(
                    "INSERT INTO _field_stats (table_name, field, total_length)
                     VALUES ('articles', 'title', -2)",
                    [],
                )?;
                connection.execute(
                    &format!(
                        "INSERT INTO {} (term, block_idx, max_score) VALUES ('rust', -1, 1.0)",
                        quote_ident(&block_table)
                    ),
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let length_error = idx.get_doc_length(1, "title").unwrap_err();
        assert!(length_error
            .to_string()
            .contains("negative document length -2"));
        let bulk_error = idx.get_doc_lengths_bulk(&[1], "title").unwrap_err();
        assert!(bulk_error
            .to_string()
            .contains("negative document length -2"));
        let total_error = idx.total_field_length("title").unwrap_err();
        assert!(total_error
            .to_string()
            .contains("negative total field length -2"));
        let block_error = idx.get_all_block_max_scores("title", "rust").unwrap_err();
        assert!(block_error.to_string().contains("invalid block index -1"));
    }

    #[test]
    fn document_ids_beyond_sqlite_integer_range_are_rejected_before_io() {
        let mut idx = idx();
        let add_error = idx
            .add_document(u64::MAX, fields([("title", "rust")]))
            .unwrap_err();
        assert!(add_error
            .to_string()
            .contains("exceeds the SQLite INTEGER range"));
        assert_eq!(idx.doc_count().unwrap(), 0);

        let lookup_error = idx.get_doc_length(u64::MAX, "title").unwrap_err();
        assert!(lookup_error
            .to_string()
            .contains("exceeds the SQLite INTEGER range"));
    }

    #[test]
    fn position_count_matches_zero_based_u32_format() {
        validate_position_count(u64::from(u32::MAX) + 1).unwrap();
        let error = validate_position_count(u64::from(u32::MAX) + 2).unwrap_err();
        assert!(error.to_string().contains("u32 index format"));
    }

    #[test]
    fn corrupt_total_rejects_remove_without_partial_delete() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust")])).unwrap();
        idx.conn
            .with(|connection| {
                connection.execute(
                    "UPDATE _field_stats SET total_length = 0
                     WHERE table_name = 'articles' AND field = 'title'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let error = idx.remove_document(1).unwrap_err();
        assert!(error.to_string().contains("underflow"));
        assert_eq!(idx.doc_count().unwrap(), 1);
        assert_eq!(idx.get_doc_length(1, "title").unwrap(), 1);
        assert_eq!(idx.doc_freq("title", "rust").unwrap(), 1);
    }

    #[test]
    fn sqlite_integer_counter_overflow_preserves_existing_index() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust")])).unwrap();
        idx.conn
            .with(|connection| {
                connection.execute(
                    "UPDATE _field_stats SET total_length = ?1
                     WHERE table_name = 'articles' AND field = 'title'",
                    [i64::MAX],
                )?;
                Ok(())
            })
            .unwrap();

        let error = idx
            .add_document(2, fields([("title", "sqlite")]))
            .unwrap_err();
        assert!(error.to_string().contains("SQLite INTEGER range"));
        assert_eq!(idx.doc_count().unwrap(), 1);
        assert_eq!(idx.doc_freq("title", "rust").unwrap(), 1);
        assert_eq!(idx.doc_freq("title", "sqlite").unwrap(), 0);
    }

    #[test]
    fn rebuild_analysis_failure_preserves_existing_index() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust")])).unwrap();
        idx.set_field_analyzer(
            "body",
            Analyzer::new(
                Tokenizer::NGram {
                    min_gram: 0,
                    max_gram: 1,
                },
                Vec::new(),
                Vec::new(),
            ),
            AnalyzerPhase::Index,
        )
        .unwrap();

        let error = idx
            .try_rebuild_documents(vec![
                (2, fields([("title", "sqlite")])),
                (3, fields([("body", "failure")])),
            ])
            .unwrap_err();
        assert!(error.to_string().contains("gram"));
        assert_eq!(idx.doc_count().unwrap(), 1);
        assert_eq!(idx.doc_freq("title", "rust").unwrap(), 1);
        assert_eq!(idx.doc_freq("title", "sqlite").unwrap(), 0);
    }

    #[test]
    fn rebuild_duplicate_document_uses_only_final_lengths() {
        let mut idx = idx();
        idx.try_rebuild_documents(vec![
            (1, fields([("title", "old old")])),
            (1, fields([("title", "new")])),
        ])
        .unwrap();

        assert_eq!(idx.doc_count().unwrap(), 1);
        assert_eq!(idx.total_field_length("title").unwrap(), 1);
        assert_eq!(idx.doc_freq("title", "old").unwrap(), 0);
        assert_eq!(idx.doc_freq("title", "new").unwrap(), 1);
    }

    #[test]
    fn rebuild_documents_replaces_postings_and_stats() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "old rust")]))
            .unwrap();

        idx.try_rebuild_documents(vec![
            (2, fields([("title", "new search")])),
            (3, fields([("title", "new rust search")])),
        ])
        .unwrap();

        assert!(idx.get_posting_list("title", "old").unwrap().is_empty());
        assert_eq!(
            idx.get_posting_list("title", "new")
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(idx.doc_length_count(Some("title")).unwrap(), 2);
        assert_eq!(idx.total_field_length("title").unwrap(), 5);
    }

    #[test]
    fn stats_match_memory_backend() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust language")]))
            .unwrap();
        idx.add_document(2, fields([("title", "rust")])).unwrap();
        let s = idx.stats().unwrap();
        assert_eq!(s.total_docs, 2);
        // After standard analyzer "rust language" -> ["rust", "languag"] (2)
        // and "rust" -> ["rust"] (1). avg = 3/2 = 1.5.
        assert!((s.avg_doc_length - 1.5).abs() < 1e-9);
        assert_eq!(s.doc_freq("title", "rust"), 2);
    }

    #[test]
    fn replacing_doc_replaces_postings() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust")])).unwrap();
        idx.add_document(1, fields([("title", "go")])).unwrap();
        assert_eq!(idx.doc_freq("title", "rust").unwrap(), 0);
        assert_eq!(idx.doc_freq("title", "go").unwrap(), 1);
        assert_eq!(idx.doc_count().unwrap(), 1);
    }

    #[test]
    fn remove_document_zeros_state() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust")])).unwrap();
        idx.add_document(2, fields([("title", "rust")])).unwrap();
        idx.remove_document(1).unwrap();
        assert_eq!(idx.doc_freq("title", "rust").unwrap(), 1);
        assert_eq!(idx.doc_count().unwrap(), 1);
        assert_eq!(idx.total_field_length("title").unwrap(), 1);
    }
}
