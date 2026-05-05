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

use crate::inverted_index::InvertedIndex;
use crate::sqlite::connection::{ManagedConnection, Result as SQLiteResult};

#[derive(Clone)]
pub struct SQLiteInvertedIndex {
    conn: ManagedConnection,
    table: String,
    analyzer: Analyzer,
}

impl SQLiteInvertedIndex {
    pub fn new(conn: ManagedConnection, table: impl Into<String>, analyzer: Analyzer) -> Self {
        Self {
            conn,
            table: table.into(),
            analyzer,
        }
    }

    fn add_document_inner(
        &self,
        doc_id: DocId,
        fields: BTreeMap<FieldName, String>,
    ) -> SQLiteResult<()> {
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
                let tokens = self.analyzer.analyze(&text);
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
