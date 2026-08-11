//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Auxiliary schema, field analysis, and skip-pointer rebuilds.

use super::{
    encode_index_u64, encode_index_usize, params, quote_ident, usize_to_index_u64,
    validate_position_count, BTreeMap, DocId, FieldName, InvertedIndex, OptionalExtension,
    SQLiteError, SQLiteInvertedIndex, SQLiteResult, StagedField, StorageBackendResult,
};

impl SQLiteInvertedIndex {
    pub(super) fn has_field(&self, field: &str) -> SQLiteResult<bool> {
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

    pub(super) fn terms_for_field(&self, field: &str) -> StorageBackendResult<Vec<String>> {
        Ok(self.conn.with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT term FROM _posting_clusters
                     WHERE table_name = ?1 AND field = ?2
                     ORDER BY term",
            )?;
            let rows = stmt
                .query_map(params![self.table, field], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?)
    }

    pub(super) fn fields_with_blockmax_tables(&self) -> StorageBackendResult<Vec<String>> {
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

    pub(super) fn ensure_aux_tables(&self, field: &str) -> SQLiteResult<()> {
        let skip_table = self.skip_table_name(field);
        let block_table = self.blockmax_table_name(field);
        self.conn
            .with(|conn| Self::ensure_aux_tables_on(conn, &skip_table, &block_table))
    }

    pub(super) fn ensure_aux_tables_on(
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

    pub(super) fn analyze_fields(
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
                postings.push((term, positions));
            }
            staged.insert(field, StagedField { length, postings });
        }
        Ok(staged)
    }

    pub(super) fn rebuild_skip_pointers_for_field(&self, field: &str) -> SQLiteResult<()> {
        if !self.has_field(field)? {
            return Ok(());
        }
        self.ensure_aux_tables(field)?;
        let table = self.skip_table_name(field);
        let mut by_term: BTreeMap<String, Vec<DocId>> = BTreeMap::new();
        for term in self
            .terms_for_field(field)
            .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?
        {
            let mut cursor = self
                .posting_cursor(field, &term)
                .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
            while let Some(entry) = cursor.current() {
                by_term.entry(term.clone()).or_default().push(entry.doc_id);
                cursor
                    .advance()
                    .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
            }
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
}
