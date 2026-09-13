//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Auxiliary schema, field analysis, and skip-pointer rebuilds.

use super::{
    encode_index_u64, encode_index_usize, params, quote_ident, BTreeMap, DocId, InvertedIndex,
    OptionalExtension, SQLiteError, SQLiteInvertedIndex, SQLiteResult, StorageBackendResult,
    TokenTermKey,
};

impl SQLiteInvertedIndex {
    pub(super) fn has_field(&self, field: &str) -> SQLiteResult<bool> {
        self.conn.with(|conn| {
            let found: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM _occurrence_lengths
                     WHERE table_name = ?1 AND field = ?2 LIMIT 1",
                    params![self.table, field],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(found.is_some())
        })
    }

    pub(super) fn terms_for_field(&self, field: &str) -> StorageBackendResult<Vec<String>> {
        self.vocabulary_keys(field)?
            .into_iter()
            .map(|term| Ok(term.to_term().into_string()?))
            .collect()
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
                    term BLOB NOT NULL,
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
                    term BLOB NOT NULL,
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

    pub(super) fn rebuild_skip_pointers_for_field(&self, field: &str) -> SQLiteResult<()> {
        if !self.has_field(field)? {
            return Ok(());
        }
        self.ensure_aux_tables(field)?;
        let table = self.skip_table_name(field);
        let mut by_term: BTreeMap<TokenTermKey, Vec<DocId>> = BTreeMap::new();
        for term in self
            .vocabulary_keys(field)
            .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?
        {
            let mut cursor = self
                .posting_cursor_key(field, &term)
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
                            params![term.as_bytes(), doc_id, skip_offset],
                        )?;
                    }
                }
            }
            tx.commit()?;
            Ok(())
        })
    }
}
