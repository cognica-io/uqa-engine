//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent brute-force vector index and exact search contract.

use super::native::NativeVectorRead;
use super::{
    blob_to_vector, cosine_similarity, decode_doc_id, encode_doc_id, i64_to_usize, params,
    select_top_k_scored, usize_to_u64, validate_persisted_ordinal_sequence,
    validate_vector_ordinal_count, validate_vector_values, vector_to_blob, Arc, DocId,
    EncodedDocVectors, ManagedConnection, Payload, PostingEntry, PostingList, SQLiteError,
    SQLiteResult, StorageBackendResult, VectorIndex,
};

#[derive(Clone)]
pub struct SQLiteVectorIndex {
    pub(super) conn: ManagedConnection,
    pub(super) table: String,
    pub(super) field: String,
    pub(super) dimensions: u32,
    pub(super) retained: Option<Arc<crate::mvcc::native::NativeSnapshot>>,
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
            retained: None,
        }
    }

    fn with_vectors<R>(
        &self,
        read: impl FnOnce(&[(DocId, u32, Vec<f32>)]) -> SQLiteResult<R>,
    ) -> SQLiteResult<R> {
        if let Some(snapshot) = self.native_snapshot()? {
            let native = NativeVectorRead::new(&snapshot, self)?;
            read(&native.vectors()?)
        } else {
            read(&self.load_all_with_ordinals()?)
        }
    }

    pub(super) fn load_all_with_ordinals(&self) -> SQLiteResult<Vec<(DocId, u32, Vec<f32>)>> {
        self.conn
            .with(|connection| self.load_all_with_ordinals_from(connection))
    }

    pub(super) fn load_all_with_ordinals_from(
        &self,
        connection: &rusqlite::Connection,
    ) -> SQLiteResult<Vec<(DocId, u32, Vec<f32>)>> {
        let mut stmt = connection.prepare(
            "SELECT doc_id, vector_ordinal, vector FROM _vectors
                 WHERE table_name = ?1 AND field = ?2
                 ORDER BY doc_id, vector_ordinal",
        )?;
        let rows = stmt.query_map(params![self.table, self.field], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (doc_id, ordinal, blob) = row?;
            let ordinal = u32::try_from(ordinal).map_err(|_| {
                SQLiteError::StorageBackend(format!(
                    "invalid vector ordinal {ordinal} for {}.{}",
                    self.table, self.field
                ))
            })?;
            let vector = blob_to_vector(&blob)?;
            self.validate_dimensions_sqlite(&vector)?;
            out.push((decode_doc_id(doc_id)?, ordinal, vector));
        }
        validate_persisted_ordinal_sequence(&out)?;
        Ok(out)
    }

    pub(super) fn validate_dimensions_sqlite(&self, vector: &[f32]) -> SQLiteResult<()> {
        validate_vector_values(self.dimensions, vector).map_err(|error| {
            SQLiteError::StorageBackend(format!(
                "invalid vector for {}.{}: {error}",
                self.table, self.field
            ))
        })
    }

    pub(super) fn validate_dimensions(&self, vector: &[f32]) -> StorageBackendResult<()> {
        Ok(self.validate_dimensions_sqlite(vector)?)
    }

    pub(super) fn stage_doc_vectors(
        &self,
        doc_id: DocId,
        vectors: &[Vec<f32>],
    ) -> SQLiteResult<EncodedDocVectors> {
        for vector in vectors {
            self.validate_dimensions_sqlite(vector)?;
        }
        validate_vector_ordinal_count(usize_to_u64("vector count", vectors.len())?)?;
        let doc_id = encode_doc_id(doc_id)?;
        let encoded = vectors
            .iter()
            .enumerate()
            .map(|(ordinal, vector)| {
                let ordinal = u32::try_from(ordinal).map_err(|_| {
                    SQLiteError::StorageBackend(
                        "vector ordinal exceeds the u32 index format".into(),
                    )
                })?;
                Ok((i64::from(ordinal), vector_to_blob(vector)?))
            })
            .collect::<SQLiteResult<Vec<_>>>()?;
        Ok((doc_id, encoded))
    }
}

impl VectorIndex for SQLiteVectorIndex {
    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    fn index_kind(&self) -> &'static str {
        "sqlite-bruteforce"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.add_many(doc_id, vec![vector])
    }

    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        let (doc_id, encoded_vectors) = self.stage_doc_vectors(doc_id, &vectors)?;
        if self
            .write_native(|read, batch| read.replace(batch, doc_id, &encoded_vectors))?
            .is_some()
        {
            return Ok(());
        }
        self.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "DELETE FROM _vectors
                 WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
                params![self.table, self.field, doc_id],
            )?;
            let mut stmt = tx.prepare(
                "INSERT INTO _vectors (table_name, field, doc_id, vector_ordinal, vector)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (ordinal, vector) in &encoded_vectors {
                stmt.execute(params![self.table, self.field, doc_id, ordinal, vector,])?;
            }
            drop(stmt);
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let doc_id = encode_doc_id(doc_id)?;
        if self
            .write_native(|read, batch| read.delete(batch, doc_id))?
            .is_some()
        {
            return Ok(());
        }
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _vectors
                 WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
                params![self.table, self.field, doc_id],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        if self
            .write_native(|read, batch| {
                read.clear_family(batch, crate::mvcc::native::NativeRecordFamily::Vectors)
            })?
            .is_some()
        {
            return Ok(());
        }
        self.conn.with(|c| {
            c.execute(
                "DELETE FROM _vectors WHERE table_name = ?1 AND field = ?2",
                params![self.table, self.field],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.validate_dimensions(query)?;
        if k == 0 {
            return Ok(PostingList::new());
        }
        Ok(self.with_vectors(|entries| {
            if entries.is_empty() {
                return Ok(PostingList::new());
            }
            let mut best_by_doc: std::collections::BTreeMap<DocId, f32> =
                std::collections::BTreeMap::new();
            for (doc_id, _, vector) in entries {
                let sim = cosine_similarity(query, vector);
                best_by_doc
                    .entry(*doc_id)
                    .and_modify(|best| {
                        if sim > *best {
                            *best = sim;
                        }
                    })
                    .or_insert(sim);
            }
            let mut scored: Vec<(DocId, f32)> = best_by_doc.into_iter().collect();
            select_top_k_scored(&mut scored, k);
            scored.sort_by_key(|(id, _)| *id);
            let entries: Vec<PostingEntry> = scored
                .into_iter()
                .map(|(doc_id, sim)| PostingEntry::new(doc_id, Payload::with_score(f64::from(sim))))
                .collect();
            Ok(PostingList::from_sorted_unchecked(entries))
        })?)
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.validate_dimensions(query)?;
        if !threshold.is_finite() {
            return Err(uqa_storage::StorageBackendError::Other(format!(
                "vector similarity threshold must be finite, got {threshold}"
            )));
        }
        Ok(self.with_vectors(|entries| {
            let mut best_by_doc: std::collections::BTreeMap<DocId, f32> =
                std::collections::BTreeMap::new();
            for (doc_id, _, vector) in entries {
                let sim = cosine_similarity(query, vector);
                if sim >= threshold {
                    best_by_doc
                        .entry(*doc_id)
                        .and_modify(|best| {
                            if sim > *best {
                                *best = sim;
                            }
                        })
                        .or_insert(sim);
                }
            }
            let mut out: Vec<PostingEntry> = best_by_doc
                .into_iter()
                .map(|(doc_id, sim)| PostingEntry::new(doc_id, Payload::with_score(f64::from(sim))))
                .collect();
            out.sort_by_key(|e| e.doc_id);
            Ok(PostingList::from_sorted_unchecked(out))
        })?)
    }

    fn count(&self) -> StorageBackendResult<usize> {
        if let Some(snapshot) = self.native_snapshot()? {
            return Ok(NativeVectorRead::new(&snapshot, self)?.count()?);
        }
        Ok(self.conn.with(|c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM _vectors WHERE table_name = ?1 AND field = ?2",
                params![self.table, self.field],
                |r| r.get(0),
            )?;
            i64_to_usize("vector count", n)
        })?)
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.retained_snapshot()?))
    }
}
