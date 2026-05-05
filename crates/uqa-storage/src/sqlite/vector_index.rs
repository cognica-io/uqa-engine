//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite`-backed [`VectorIndex`] (brute-force; persistent).

use std::sync::Arc;

use rusqlite::params;
use uqa_core::{DocId, Payload, PostingEntry, PostingList};

use crate::sqlite::connection::{ManagedConnection, Result as SQLiteResult};
use crate::vector_index::{cosine_similarity, VectorIndex};

#[derive(Clone)]
pub struct SQLiteVectorIndex {
    conn: ManagedConnection,
    table: String,
    field: String,
    dimensions: u32,
}

impl SQLiteVectorIndex {
    pub fn new(
        conn: ManagedConnection,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
    ) -> Self {
        Self {
            conn,
            table: table.into(),
            field: field.into(),
            dimensions,
        }
    }

    fn load_all(&self) -> SQLiteResult<Vec<(DocId, Vec<f32>)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT doc_id, vector FROM _vectors
                 WHERE table_name = ?1 AND field = ?2
                 ORDER BY doc_id",
            )?;
            let rows = stmt.query_map(params![self.table, self.field], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (doc_id, blob) = row?;
                out.push((doc_id as DocId, blob_to_vector(&blob)));
            }
            Ok(out)
        })
    }
}

fn vector_to_blob(v: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(v.len() * 4);
    for x in v {
        buf.extend_from_slice(&x.to_le_bytes());
    }
    buf
}

fn blob_to_vector(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

impl VectorIndex for SQLiteVectorIndex {
    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) {
        debug_assert_eq!(
            vector.len() as u32,
            self.dimensions,
            "vector dimension mismatch"
        );
        let blob = vector_to_blob(&vector);
        let _ = self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _vectors (table_name, field, doc_id, vector)
                 VALUES (?1, ?2, ?3, ?4)",
                params![self.table, self.field, doc_id as i64, blob],
            )?;
            Ok(())
        });
    }

    fn delete(&mut self, doc_id: DocId) {
        let _ = self.conn.with(|c| {
            c.execute(
                "DELETE FROM _vectors
                 WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
                params![self.table, self.field, doc_id as i64],
            )?;
            Ok(())
        });
    }

    fn clear(&mut self) {
        let _ = self.conn.with(|c| {
            c.execute(
                "DELETE FROM _vectors WHERE table_name = ?1 AND field = ?2",
                params![self.table, self.field],
            )?;
            Ok(())
        });
    }

    fn search_knn(&self, query: &[f32], k: usize) -> PostingList {
        if k == 0 {
            return PostingList::new();
        }
        let entries = self.load_all().unwrap_or_default();
        if entries.is_empty() {
            return PostingList::new();
        }
        let mut scored: Vec<(DocId, f32)> = entries
            .iter()
            .map(|(doc_id, v)| (*doc_id, cosine_similarity(query, v)))
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(k);
        scored.sort_by_key(|(id, _)| *id);
        let entries: Vec<PostingEntry> = scored
            .into_iter()
            .map(|(doc_id, sim)| PostingEntry::new(doc_id, Payload::with_score(f64::from(sim))))
            .collect();
        PostingList::from_sorted_unchecked(entries)
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> PostingList {
        let entries = self.load_all().unwrap_or_default();
        let mut out: Vec<PostingEntry> = entries
            .iter()
            .filter_map(|(doc_id, v)| {
                let sim = cosine_similarity(query, v);
                if sim >= threshold {
                    Some(PostingEntry::new(
                        *doc_id,
                        Payload::with_score(f64::from(sim)),
                    ))
                } else {
                    None
                }
            })
            .collect();
        out.sort_by_key(|e| e.doc_id);
        PostingList::from_sorted_unchecked(out)
    }

    fn count(&self) -> usize {
        self.conn
            .with(|c| {
                let n: i64 = c.query_row(
                    "SELECT COUNT(*) FROM _vectors WHERE table_name = ?1 AND field = ?2",
                    params![self.table, self.field],
                    |r| r.get(0),
                )?;
                Ok(n as usize)
            })
            .unwrap_or(0)
    }

    fn snapshot(&self) -> Arc<dyn VectorIndex> {
        Arc::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::catalog::Catalog;

    fn idx() -> SQLiteVectorIndex {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _cat = Catalog::open(mc.clone()).unwrap();
        SQLiteVectorIndex::new(mc, "articles", "embedding", 3)
    }

    #[test]
    fn add_search_round_trip() {
        let mut idx = idx();
        idx.add(1, vec![1.0, 0.0, 0.0]);
        idx.add(2, vec![0.0, 1.0, 0.0]);
        idx.add(3, vec![0.7, 0.7, 0.0]);
        let pl = idx.search_knn(&[1.0, 0.0, 0.0], 2);
        let docs: Vec<_> = pl.doc_ids().collect();
        assert_eq!(docs, vec![1, 3]);
    }

    #[test]
    fn delete_removes_vector() {
        let mut idx = idx();
        idx.add(1, vec![1.0, 0.0, 0.0]);
        idx.delete(1);
        assert_eq!(idx.count(), 0);
    }

    #[test]
    fn round_trip_blob_preserves_bits() {
        let v = vec![0.1f32, -3.5, 12345.678];
        assert_eq!(blob_to_vector(&vector_to_blob(&v)), v);
    }
}
