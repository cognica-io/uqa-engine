//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Skip-pointer and scorer-versioned block-max materialization.

use super::{
    decode_index_u64, decode_index_usize, encode_index_u64, encode_index_usize, params,
    quote_ident, table_exists, usize_to_index_u64, BlockMaxIndex, BlockMaxScorer, DocId,
    InvertedIndex, OptionalExtension, SQLiteError, SQLiteInvertedIndex, StorageBackendResult,
};

impl SQLiteInvertedIndex {
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
}
