//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite`-backed [`InvertedIndex`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use uqa_analysis::Analyzer;
use uqa_core::{DocId, FieldName, IndexStats, Payload, PostingEntry, PostingList};

use crate::block_max_index::{BlockMaxIndex, BlockMaxScorer, DEFAULT_BLOCK_SIZE};
use crate::inverted_index::{AnalyzerPhase, InvertedIndex};
use crate::sqlite::connection::{ManagedConnection, Result as SQLiteResult};

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
    /// Python's `SQLiteInvertedIndex._tokenize`.
    pub fn tokenize(&self, text: &str, field: &str) -> Vec<String> {
        let analyzer = self
            .index_field_analyzers
            .get(field)
            .unwrap_or(&self.analyzer);
        analyzer.analyze(text)
    }

    pub fn skip_table_name(&self, field: &str) -> String {
        format!("_skip_{}_{}", self.table, field)
    }

    pub fn blockmax_table_name(&self, field: &str) -> String {
        format!("_blockmax_{}_{}", self.table, field)
    }

    pub fn flush_skip_pointers(&self) {
        let fields = self.field_names();
        for field in fields {
            let _ = self.rebuild_skip_pointers_for_field(&field);
        }
    }

    pub fn skip_to(&self, field: &str, term: &str, target_doc_id: DocId) -> (DocId, usize) {
        let _ = self.rebuild_skip_pointers_for_field(field);
        let table = self.skip_table_name(field);
        self.conn
            .with(|conn| {
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
                    .query_row(&sql, params![term, target_doc_id as i64], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .optional()?;
                Ok(row
                    .map(|(doc, off)| (doc.max(0) as DocId, off.max(0) as usize))
                    .unwrap_or((0, 0)))
            })
            .unwrap_or((0, 0))
    }

    pub fn build_block_max_scores<S: BlockMaxScorer + ?Sized>(
        &self,
        field: &str,
        term: &str,
        scorer: &S,
    ) {
        let posting_list = self.get_posting_list(field, term);
        if posting_list.is_empty() && !self.has_field(field) {
            return;
        }
        let scored_entries = posting_list
            .entries()
            .iter()
            .map(|entry| {
                let tf = entry.payload.positions.len().max(1) as u64;
                let doc_length = self.get_doc_length(entry.doc_id, field).max(tf);
                (tf, doc_length)
            })
            .collect::<Vec<_>>();
        let _ = self.ensure_aux_tables(field);
        let table = self.blockmax_table_name(field);
        let _ = self.conn.with_mut(|conn| {
            let tx = conn.transaction()?;
            tx.execute(
                &format!("DELETE FROM {} WHERE term = ?1", quote_ident(&table)),
                [term],
            )?;
            let df = posting_list.len() as u64;
            for (block_idx, chunk) in scored_entries.chunks(Self::BLOCK_SIZE).enumerate() {
                let mut max_score = 0.0_f64;
                for &(tf, doc_length) in chunk {
                    max_score = max_score.max(scorer.score(tf, doc_length, df));
                }
                tx.execute(
                    &format!(
                        "INSERT OR REPLACE INTO {}
                            (term, block_idx, max_score)
                         VALUES (?1, ?2, ?3)",
                        quote_ident(&table)
                    ),
                    params![term, block_idx as i64, max_score],
                )?;
            }
            tx.commit()?;
            Ok(())
        });
    }

    pub fn build_all_block_max_scores<S: BlockMaxScorer + ?Sized>(&self, field: &str, scorer: &S) {
        let terms = self.terms_for_field(field);
        for term in terms {
            self.build_block_max_scores(field, &term, scorer);
        }
    }

    pub fn get_block_max_score(&self, field: &str, term: &str, block_idx: usize) -> f64 {
        let table = self.blockmax_table_name(field);
        self.conn
            .with(|conn| {
                if !table_exists(conn, &table)? {
                    return Ok(0.0);
                }
                let sql = format!(
                    "SELECT max_score FROM {}
                     WHERE term = ?1 AND block_idx = ?2",
                    quote_ident(&table)
                );
                let score: Option<f64> = conn
                    .query_row(&sql, params![term, block_idx as i64], |row| row.get(0))
                    .optional()?;
                Ok(score.unwrap_or(0.0))
            })
            .unwrap_or(0.0)
    }

    pub fn get_all_block_max_scores(&self, field: &str, term: &str) -> Vec<f64> {
        let table = self.blockmax_table_name(field);
        self.conn
            .with(|conn| {
                if !table_exists(conn, &table)? {
                    return Ok(Vec::new());
                }
                let sql = format!(
                    "SELECT max_score FROM {}
                     WHERE term = ?1 ORDER BY block_idx",
                    quote_ident(&table)
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map([term], |row| row.get::<_, f64>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .unwrap_or_default()
    }

    pub fn load_block_max_into(&self, target: &mut BlockMaxIndex) {
        for field in self.fields_with_blockmax_tables() {
            for term in self.terms_for_field(&field) {
                let scores = self.get_all_block_max_scores(&field, &term);
                if !scores.is_empty() {
                    target.set_block_maxes(&self.table, &field, &term, scores);
                }
            }
        }
    }

    fn has_field(&self, field: &str) -> bool {
        self.field_names().iter().any(|f| f == field)
    }

    fn terms_for_field(&self, field: &str) -> Vec<String> {
        self.conn
            .with(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT term FROM _postings
                     WHERE table_name = ?1 AND field = ?2
                     ORDER BY term",
                )?;
                let rows = stmt
                    .query_map(params![self.table, field], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .unwrap_or_default()
    }

    fn fields_with_blockmax_tables(&self) -> Vec<String> {
        let prefix = format!("_blockmax_{}_", self.table);
        self.conn
            .with(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT name FROM sqlite_master
                     WHERE type = 'table' AND name LIKE ?1
                     ORDER BY name",
                )?;
                let rows = stmt
                    .query_map([format!("{prefix}%")], |row| row.get::<_, String>(0))?
                    .filter_map(Result::ok)
                    .filter_map(|name| name.strip_prefix(&prefix).map(str::to_string))
                    .collect::<Vec<_>>();
                Ok(rows)
            })
            .unwrap_or_default()
    }

    fn ensure_aux_tables(&self, field: &str) -> SQLiteResult<()> {
        let skip_table = self.skip_table_name(field);
        let block_table = self.blockmax_table_name(field);
        self.conn.with(|conn| {
            conn.execute(
                &format!(
                    "CREATE TABLE IF NOT EXISTS {} (
                        term TEXT NOT NULL,
                        skip_doc_id INTEGER NOT NULL,
                        skip_offset INTEGER NOT NULL,
                        PRIMARY KEY (term, skip_doc_id)
                    )",
                    quote_ident(&skip_table)
                ),
                [],
            )?;
            conn.execute(
                &format!(
                    "CREATE TABLE IF NOT EXISTS {} (
                        term TEXT NOT NULL,
                        block_idx INTEGER NOT NULL,
                        max_score REAL NOT NULL,
                        PRIMARY KEY (term, block_idx)
                    )",
                    quote_ident(&block_table)
                ),
                [],
            )?;
            Ok(())
        })
    }

    fn rebuild_skip_pointers_for_field(&self, field: &str) -> SQLiteResult<()> {
        if !self.has_field(field) {
            return Ok(());
        }
        self.ensure_aux_tables(field)?;
        let table = self.skip_table_name(field);
        let postings: Vec<(String, DocId)> = self.conn.with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT term, doc_id FROM _postings
                 WHERE table_name = ?1 AND field = ?2
                 ORDER BY term, doc_id",
            )?;
            let rows = stmt
                .query_map(params![self.table, field], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as DocId))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        let mut by_term: BTreeMap<String, Vec<DocId>> = BTreeMap::new();
        for (term, doc_id) in postings {
            by_term.entry(term).or_default().push(doc_id);
        }
        self.conn.with_mut(|conn| {
            let tx = conn.transaction()?;
            tx.execute(&format!("DELETE FROM {}", quote_ident(&table)), [])?;
            for (term, docs) in by_term {
                for (block_idx, chunk) in docs.chunks(Self::BLOCK_SIZE).enumerate() {
                    if let Some(doc_id) = chunk.first() {
                        tx.execute(
                            &format!(
                                "INSERT OR REPLACE INTO {}
                                    (term, skip_doc_id, skip_offset)
                                 VALUES (?1, ?2, ?3)",
                                quote_ident(&table)
                            ),
                            params![term, *doc_id as i64, (block_idx * Self::BLOCK_SIZE) as i64],
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
        for field in fields.keys() {
            self.ensure_aux_tables(field)?;
        }
        self.conn.with_mut(|conn| {
            let tx = conn.transaction()?;
            // Replacing an existing doc: drop its prior postings + lengths.
            tx.execute(
                "DELETE FROM _postings WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, doc_id as i64],
            )?;
            // Subtract old lengths from _field_stats before deleting.
            {
                let mut stmt = tx.prepare(
                    "SELECT field, length FROM _doc_lengths
                     WHERE table_name = ?1 AND doc_id = ?2",
                )?;
                let rows = stmt
                    .query_map(params![self.table, doc_id as i64], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for (field, length) in rows {
                    tx.execute(
                        "UPDATE _field_stats
                         SET total_length = MAX(0, total_length - ?3)
                         WHERE table_name = ?1 AND field = ?2",
                        params![self.table, field, length],
                    )?;
                }
            }
            tx.execute(
                "DELETE FROM _doc_lengths WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, doc_id as i64],
            )?;

            for (field, text) in fields {
                let analyzer = self
                    .index_field_analyzers
                    .get(&field)
                    .unwrap_or(&self.analyzer);
                let tokens = analyzer.analyze(&text);
                let length = tokens.len() as i64;
                tx.execute(
                    "INSERT OR REPLACE INTO _doc_lengths
                        (table_name, doc_id, field, length)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![self.table, doc_id as i64, field, length],
                )?;
                tx.execute(
                    "INSERT INTO _field_stats (table_name, field, total_length)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(table_name, field) DO UPDATE
                        SET total_length = total_length + excluded.total_length",
                    params![self.table, field, length],
                )?;

                let mut term_positions: BTreeMap<String, Vec<u32>> = BTreeMap::new();
                for (pos, token) in tokens.into_iter().enumerate() {
                    term_positions.entry(token).or_default().push(pos as u32);
                }
                for (term, mut positions) in term_positions {
                    positions.sort_unstable();
                    positions.dedup();
                    let blob = positions_to_blob(&positions);
                    tx.execute(
                        "INSERT OR REPLACE INTO _postings
                            (table_name, field, term, doc_id, positions)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![self.table, field, term, doc_id as i64, blob],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    fn remove_document_inner(&self, doc_id: DocId) -> SQLiteResult<()> {
        self.conn.with_mut(|conn| {
            let tx = conn.transaction()?;
            // Subtract length contributions from _field_stats.
            let mut stmt = tx.prepare(
                "SELECT field, length FROM _doc_lengths
                 WHERE table_name = ?1 AND doc_id = ?2",
            )?;
            let rows = stmt
                .query_map(params![self.table, doc_id as i64], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(stmt);
            for (field, length) in rows {
                tx.execute(
                    "UPDATE _field_stats
                     SET total_length = MAX(0, total_length - ?3)
                     WHERE table_name = ?1 AND field = ?2",
                    params![self.table, field, length],
                )?;
            }
            tx.execute(
                "DELETE FROM _doc_lengths WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, doc_id as i64],
            )?;
            tx.execute(
                "DELETE FROM _postings WHERE table_name = ?1 AND doc_id = ?2",
                params![self.table, doc_id as i64],
            )?;
            tx.commit()?;
            Ok(())
        })
    }
}

fn positions_to_blob(positions: &[u32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(positions.len() * 4);
    for p in positions {
        buf.extend_from_slice(&p.to_le_bytes());
    }
    buf
}

fn blob_to_positions(blob: &[u8]) -> Vec<u32> {
    blob.chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
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

    fn add_document(&mut self, doc_id: DocId, fields: BTreeMap<FieldName, String>) {
        let _ = self.add_document_inner(doc_id, fields);
    }

    fn remove_document(&mut self, doc_id: DocId) {
        let _ = self.remove_document_inner(doc_id);
    }

    fn clear(&mut self) {
        let _ = self.conn.with(|c| {
            c.execute(
                "DELETE FROM _postings WHERE table_name = ?1",
                params![self.table],
            )?;
            c.execute(
                "DELETE FROM _doc_lengths WHERE table_name = ?1",
                params![self.table],
            )?;
            c.execute(
                "DELETE FROM _field_stats WHERE table_name = ?1",
                params![self.table],
            )?;
            Ok(())
        });
    }

    fn get_posting_list(&self, field: &str, term: &str) -> PostingList {
        self.conn
            .with(|c| {
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
                    let positions = blob_to_positions(&blob);
                    entries.push(PostingEntry::new(
                        doc_id as DocId,
                        Payload {
                            positions,
                            score: 0.0,
                            fields: BTreeMap::new(),
                        },
                    ));
                }
                Ok(PostingList::from_sorted_unchecked(entries))
            })
            .unwrap_or_default()
    }

    fn doc_freq(&self, field: &str, term: &str) -> u64 {
        self.conn
            .with(|c| {
                let n: i64 = c.query_row(
                    "SELECT COUNT(*) FROM _postings
                     WHERE table_name = ?1 AND field = ?2 AND term = ?3",
                    params![self.table, field, term],
                    |r| r.get(0),
                )?;
                Ok(n as u64)
            })
            .unwrap_or(0)
    }

    fn get_doc_length(&self, doc_id: DocId, field: &str) -> u64 {
        self.conn
            .with(|c| {
                let n: Option<i64> = c
                    .query_row(
                        "SELECT length FROM _doc_lengths
                         WHERE table_name = ?1 AND doc_id = ?2 AND field = ?3",
                        params![self.table, doc_id as i64, field],
                        |r| r.get(0),
                    )
                    .optional()?;
                Ok(n.unwrap_or(0) as u64)
            })
            .unwrap_or(0)
    }

    fn get_term_freq(&self, doc_id: DocId, field: &str, term: &str) -> u64 {
        self.conn
            .with(|c| {
                let blob: Option<Vec<u8>> = c
                    .query_row(
                        "SELECT positions FROM _postings
                         WHERE table_name = ?1 AND field = ?2
                            AND term = ?3 AND doc_id = ?4",
                        params![self.table, field, term, doc_id as i64],
                        |r| r.get(0),
                    )
                    .optional()?;
                Ok(blob.map_or(0, |b| (b.len() / 4) as u64))
            })
            .unwrap_or(0)
    }

    fn doc_count(&self) -> u64 {
        self.conn
            .with(|c| {
                let n: i64 = c.query_row(
                    "SELECT COUNT(DISTINCT doc_id) FROM _doc_lengths
                     WHERE table_name = ?1",
                    params![self.table],
                    |r| r.get(0),
                )?;
                Ok(n as u64)
            })
            .unwrap_or(0)
    }

    fn total_field_length(&self, field: &str) -> u64 {
        self.conn
            .with(|c| {
                let n: Option<i64> = c
                    .query_row(
                        "SELECT total_length FROM _field_stats
                         WHERE table_name = ?1 AND field = ?2",
                        params![self.table, field],
                        |r| r.get(0),
                    )
                    .optional()?;
                Ok(n.unwrap_or(0) as u64)
            })
            .unwrap_or(0)
    }

    fn stats(&self) -> IndexStats {
        let doc_count = self.doc_count();
        let mut s = IndexStats::default();
        s.total_docs = doc_count;
        if doc_count > 0 {
            let total: u64 = self
                .conn
                .with(|c| {
                    let n: i64 = c.query_row(
                        "SELECT COALESCE(SUM(total_length), 0) FROM _field_stats
                         WHERE table_name = ?1",
                        params![self.table],
                        |r| r.get(0),
                    )?;
                    Ok(n as u64)
                })
                .unwrap_or(0);
            s.avg_doc_length = total as f64 / doc_count as f64;
        }
        // Pull all (field, term) doc-frequencies in one query.
        let pairs: Vec<(String, String, u64)> = self
            .conn
            .with(|c| {
                let mut stmt = c.prepare(
                    "SELECT field, term, COUNT(*) FROM _postings
                     WHERE table_name = ?1
                     GROUP BY field, term",
                )?;
                let rows = stmt.query_map(params![self.table], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)? as u64,
                    ))
                })?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row?);
                }
                Ok(out)
            })
            .unwrap_or_default();
        for (field, term, df) in pairs {
            s.set_doc_freq(field, term, df);
        }
        // Touch BTreeSet to silence unused import in some builds.
        let _ = std::marker::PhantomData::<BTreeSet<()>>;
        s
    }

    fn snapshot(&self) -> Arc<dyn InvertedIndex> {
        Arc::new(self.clone())
    }

    fn field_names(&self) -> Vec<FieldName> {
        self.conn
            .with(|c| {
                let mut stmt =
                    c.prepare("SELECT DISTINCT field FROM _doc_lengths WHERE table_name = ?1")?;
                let rows = stmt
                    .query_map([&self.table], |row| row.get::<_, String>(0))?
                    .filter_map(Result::ok)
                    .collect::<Vec<_>>();
                Ok(rows)
            })
            .unwrap_or_default()
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
    use uqa_analysis::analyzer::standard_analyzer;

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
        idx.add_document(1, fields([("title", "rust language")]));
        idx.add_document(2, fields([("title", "python language")]));
        let pl = idx.get_posting_list("title", "languag");
        let docs: Vec<_> = pl.doc_ids().collect();
        assert_eq!(docs, vec![1, 2]);
    }

    #[test]
    fn doc_freq_and_term_freq() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust rust rust")]));
        idx.add_document(2, fields([("title", "rust")]));
        assert_eq!(idx.doc_freq("title", "rust"), 2);
        assert_eq!(idx.get_term_freq(1, "title", "rust"), 3);
        assert_eq!(idx.get_term_freq(2, "title", "rust"), 1);
    }

    #[test]
    fn stats_match_memory_backend() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust language")]));
        idx.add_document(2, fields([("title", "rust")]));
        let s = idx.stats();
        assert_eq!(s.total_docs, 2);
        // After standard analyzer "rust language" -> ["rust", "languag"] (2)
        // and "rust" -> ["rust"] (1). avg = 3/2 = 1.5.
        assert!((s.avg_doc_length - 1.5).abs() < 1e-9);
        assert_eq!(s.doc_freq("title", "rust"), 2);
    }

    #[test]
    fn replacing_doc_replaces_postings() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust")]));
        idx.add_document(1, fields([("title", "go")]));
        assert_eq!(idx.doc_freq("title", "rust"), 0);
        assert_eq!(idx.doc_freq("title", "go"), 1);
        assert_eq!(idx.doc_count(), 1);
    }

    #[test]
    fn remove_document_zeros_state() {
        let mut idx = idx();
        idx.add_document(1, fields([("title", "rust")]));
        idx.add_document(2, fields([("title", "rust")]));
        idx.remove_document(1);
        assert_eq!(idx.doc_freq("title", "rust"), 1);
        assert_eq!(idx.doc_count(), 1);
        assert_eq!(idx.total_field_length("title"), 1);
    }
}
