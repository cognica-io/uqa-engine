//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SQLite`-backed vector indexes.

use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use uqa_core::{DocId, Payload, PostingEntry, PostingList};

use crate::ivf_index::{IVFIndex, IVFMetadataSnapshot, IVFState};
use crate::sqlite::connection::{ManagedConnection, Result as SQLiteResult, SQLiteError};
use crate::vector_index::{
    cosine_similarity, select_top_k_scored, validate_vector_values, VectorIndex,
};
use crate::StorageBackendResult;

type EncodedVector = (i64, Vec<u8>);
type EncodedDocVectors = (i64, Vec<EncodedVector>);

fn encode_doc_id(doc_id: DocId) -> SQLiteResult<i64> {
    i64::try_from(doc_id).map_err(|_| {
        SQLiteError::StorageBackend(format!(
            "document id {doc_id} does not fit in SQLite INTEGER"
        ))
    })
}

fn decode_doc_id(doc_id: i64) -> SQLiteResult<DocId> {
    DocId::try_from(doc_id).map_err(|_| {
        SQLiteError::StorageBackend(format!(
            "invalid negative document id {doc_id} in persisted vector index"
        ))
    })
}

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
        self.load_all_with_ordinals().map(|rows| {
            rows.into_iter()
                .map(|(doc_id, _, vector)| (doc_id, vector))
                .collect()
        })
    }

    fn load_all_with_ordinals(&self) -> SQLiteResult<Vec<(DocId, u32, Vec<f32>)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
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
        })
    }

    fn validate_dimensions_sqlite(&self, vector: &[f32]) -> SQLiteResult<()> {
        validate_vector_values(self.dimensions, vector).map_err(|error| {
            SQLiteError::StorageBackend(format!(
                "invalid vector for {}.{}: {error}",
                self.table, self.field
            ))
        })
    }

    fn validate_dimensions(&self, vector: &[f32]) -> StorageBackendResult<()> {
        Ok(self.validate_dimensions_sqlite(vector)?)
    }

    fn stage_doc_vectors(
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

fn vector_to_blob(v: &[f32]) -> SQLiteResult<Vec<u8>> {
    let capacity = v
        .len()
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or_else(|| SQLiteError::StorageBackend("vector payload size overflow".into()))?;
    let mut buf = Vec::with_capacity(capacity);
    for x in v {
        buf.extend_from_slice(&x.to_le_bytes());
    }
    Ok(buf)
}

fn usize_to_u64(field: &str, value: usize) -> SQLiteResult<u64> {
    u64::try_from(value).map_err(|_| {
        SQLiteError::StorageBackend(format!("{field} does not fit in the u64 counter range"))
    })
}

fn validate_vector_ordinal_count(count: u64) -> SQLiteResult<()> {
    if count > u64::from(u32::MAX) + 1 {
        return Err(SQLiteError::StorageBackend(
            "vector ordinal exceeds the u32 index format".into(),
        ));
    }
    Ok(())
}

fn validate_persisted_ordinal_sequence(rows: &[(DocId, u32, Vec<f32>)]) -> SQLiteResult<()> {
    let mut current_doc = None;
    let mut expected = 0_u64;
    for (doc_id, ordinal, _) in rows {
        if current_doc != Some(*doc_id) {
            current_doc = Some(*doc_id);
            expected = 0;
        }
        if u64::from(*ordinal) != expected {
            return Err(SQLiteError::StorageBackend(format!(
                "invalid persisted vector ordinal sequence for document {doc_id}: expected {expected}, found {ordinal}"
            )));
        }
        expected = expected.checked_add(1).ok_or_else(|| {
            SQLiteError::StorageBackend("persisted vector ordinal sequence overflow".into())
        })?;
    }
    Ok(())
}

fn blob_to_vector(blob: &[u8]) -> SQLiteResult<Vec<f32>> {
    if blob.len() % 4 != 0 {
        return Err(SQLiteError::StorageBackend(
            "invalid vector payload".to_string(),
        ));
    }
    Ok(blob
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
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
        let entries = self.load_all()?;
        if entries.is_empty() {
            return Ok(PostingList::new());
        }
        let mut best_by_doc: std::collections::BTreeMap<DocId, f32> =
            std::collections::BTreeMap::new();
        for (doc_id, vector) in &entries {
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
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.validate_dimensions(query)?;
        if !threshold.is_finite() {
            return Err(crate::StorageBackendError::Other(format!(
                "vector similarity threshold must be finite, got {threshold}"
            )));
        }
        let entries = self.load_all()?;
        let mut best_by_doc: std::collections::BTreeMap<DocId, f32> =
            std::collections::BTreeMap::new();
        for (doc_id, vector) in &entries {
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
    }

    fn count(&self) -> StorageBackendResult<usize> {
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
        Ok(Arc::new(self.clone()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SQLiteIVFParams {
    nlist: usize,
    nprobe: usize,
    train_threshold: usize,
}

impl SQLiteIVFParams {
    fn new(nlist: usize, nprobe: usize, train_threshold: usize) -> Self {
        let nlist = nlist.max(1);
        Self {
            nlist,
            nprobe: nprobe.max(1),
            train_threshold: train_threshold.max(1),
        }
    }
}

#[derive(Debug, Clone)]
struct SQLiteIVFMeta {
    dimensions: u32,
    params: SQLiteIVFParams,
    state: IVFState,
    vector_count: usize,
}

struct EncodedIVFMetadata {
    nlist: i64,
    nprobe: i64,
    train_threshold: i64,
    state: IVFState,
    trained_size: i64,
    deletes_since_train: i64,
    vector_count: i64,
    centroids: Vec<(i64, Vec<u8>)>,
    assignments: Vec<(i64, i64, i64)>,
}

#[derive(Clone)]
pub struct SQLiteIVFIndex {
    persistent: SQLiteVectorIndex,
    params: SQLiteIVFParams,
}

impl SQLiteIVFIndex {
    pub fn new(
        conn: ManagedConnection,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
    ) -> Self {
        Self {
            persistent: SQLiteVectorIndex::new(conn, table, field, dimensions),
            params: SQLiteIVFParams::new(100, 10, 256),
        }
    }

    pub fn with_params(
        conn: ManagedConnection,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        nlist: usize,
        nprobe: usize,
        train_threshold: usize,
    ) -> Self {
        Self {
            persistent: SQLiteVectorIndex::new(conn, table, field, dimensions),
            params: SQLiteIVFParams::new(nlist, nprobe, train_threshold),
        }
    }

    pub fn open_existing(
        conn: ManagedConnection,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        nlist: usize,
        nprobe: usize,
        train_threshold: usize,
    ) -> Self {
        Self {
            persistent: SQLiteVectorIndex::new(conn, table, field, dimensions),
            params: SQLiteIVFParams::new(nlist, nprobe, train_threshold),
        }
    }

    pub fn drop_metadata(conn: &ManagedConnection, table: &str, field: &str) -> SQLiteResult<()> {
        conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "DELETE FROM _ivf_indexes WHERE table_name = ?1 AND field = ?2",
                params![table, field],
            )?;
            tx.execute(
                "DELETE FROM _ivf_centroids WHERE table_name = ?1 AND field = ?2",
                params![table, field],
            )?;
            tx.execute(
                "DELETE FROM _ivf_assignments WHERE table_name = ?1 AND field = ?2",
                params![table, field],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    /// Load metadata suitable for a read-only search. Index maintenance is a
    /// write-path responsibility: a query must never retrain or persist IVF
    /// state, because doing so turns an otherwise concurrent WAL reader into
    /// a writer and can block behind an unrelated transaction.
    fn ready_meta(&self) -> StorageBackendResult<Option<SQLiteIVFMeta>> {
        let Some(mut meta) = self.load_meta()? else {
            return Ok(None);
        };
        if meta.state == IVFState::Trained
            && (meta.dimensions != self.persistent.dimensions
                || meta.params != self.params
                || meta.vector_count != self.persistent.count()?)
        {
            // Mark only the in-memory copy. `search_knn` will fall back to
            // the exact persistent scan; the next vector mutation repairs
            // and persists the metadata.
            meta.state = IVFState::Stale;
        }
        Ok(Some(meta))
    }

    fn train_metadata(&self) -> StorageBackendResult<()> {
        let entries = self.persistent.load_all_with_ordinals()?;
        if entries.len() < self.params.train_threshold {
            return self.save_untrained_metadata_with_count(entries.len());
        }

        let mut ivf = IVFIndex::with_params(
            self.persistent.dimensions,
            self.params.nlist,
            self.params.nprobe,
            self.params.train_threshold,
        );
        let mut by_doc: std::collections::BTreeMap<DocId, Vec<(u32, Vec<f32>)>> =
            std::collections::BTreeMap::new();
        for (doc_id, ordinal, vector) in entries {
            by_doc.entry(doc_id).or_default().push((ordinal, vector));
        }
        for (doc_id, mut vectors) in by_doc {
            vectors.sort_by_key(|(ordinal, _)| *ordinal);
            ivf.add_many(
                doc_id,
                vectors.into_iter().map(|(_, vector)| vector).collect(),
            )?;
        }
        ivf.train()?;
        self.save_trained_metadata(&ivf.metadata_snapshot())
    }

    fn metadata_for_entries(
        &self,
        entries: &[(DocId, u32, Vec<f32>)],
    ) -> StorageBackendResult<IVFMetadataSnapshot> {
        validate_persisted_ordinal_sequence(entries)?;
        if entries.len() < self.params.train_threshold {
            return Ok(IVFMetadataSnapshot {
                state: IVFState::Untrained,
                centroids: Vec::new(),
                assignments: Vec::new(),
                trained_size: 0,
                deletes_since_train: 0,
                vector_count: entries.len(),
            });
        }

        let mut ivf = IVFIndex::with_params(
            self.persistent.dimensions,
            self.params.nlist,
            self.params.nprobe,
            self.params.train_threshold,
        );
        let mut by_doc = std::collections::BTreeMap::<DocId, Vec<Vec<f32>>>::new();
        for (doc_id, _, vector) in entries {
            by_doc.entry(*doc_id).or_default().push(vector.clone());
        }
        for (doc_id, vectors) in by_doc {
            ivf.add_many(doc_id, vectors)?;
        }
        ivf.train()?;
        Ok(ivf.metadata_snapshot())
    }

    fn apply_doc_replacement_atomically(
        &self,
        doc_id: i64,
        encoded_vectors: &[(i64, Vec<u8>)],
        metadata: &EncodedIVFMetadata,
    ) -> StorageBackendResult<()> {
        self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "DELETE FROM _vectors
                 WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
                params![self.persistent.table, self.persistent.field, doc_id],
            )?;
            {
                let mut statement = tx.prepare(
                    "INSERT INTO _vectors
                        (table_name, field, doc_id, vector_ordinal, vector)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )?;
                for (ordinal, vector) in encoded_vectors {
                    statement.execute(params![
                        self.persistent.table,
                        self.persistent.field,
                        doc_id,
                        ordinal,
                        vector,
                    ])?;
                }
            }
            write_encoded_metadata(&tx, &self.persistent, metadata)?;
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    fn apply_doc_delete_atomically(
        &self,
        doc_id: i64,
        metadata: &EncodedIVFMetadata,
    ) -> StorageBackendResult<()> {
        self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "DELETE FROM _vectors
                 WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
                params![self.persistent.table, self.persistent.field, doc_id],
            )?;
            write_encoded_metadata(&tx, &self.persistent, metadata)?;
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    fn save_untrained_metadata(&self) -> StorageBackendResult<()> {
        self.save_untrained_metadata_with_count(self.persistent.count()?)
    }

    fn save_untrained_metadata_with_count(&self, vector_count: usize) -> StorageBackendResult<()> {
        self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "DELETE FROM _ivf_centroids WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            tx.execute(
                "DELETE FROM _ivf_assignments WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            write_meta_row(
                &tx,
                &self.persistent,
                self.params,
                IVFState::Untrained,
                0,
                0,
                vector_count,
            )?;
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    fn save_trained_metadata(&self, snapshot: &IVFMetadataSnapshot) -> StorageBackendResult<()> {
        let metadata = encode_ivf_metadata(self.params, snapshot)?;
        self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            write_encoded_metadata(&tx, &self.persistent, &metadata)?;
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    fn load_meta(&self) -> SQLiteResult<Option<SQLiteIVFMeta>> {
        let row = self.persistent.conn.with(|conn| {
            let row = conn
                .query_row(
                    "SELECT dimensions, nlist, nprobe, train_threshold, state,
                        trained_size, deletes_since_train, vector_count
                   FROM _ivf_indexes
                  WHERE table_name = ?1 AND field = ?2",
                    params![self.persistent.table, self.persistent.field],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, i64>(2)?,
                            r.get::<_, i64>(3)?,
                            r.get::<_, String>(4)?,
                            r.get::<_, i64>(5)?,
                            r.get::<_, i64>(6)?,
                            r.get::<_, i64>(7)?,
                        ))
                    },
                )
                .optional()?;
            Ok(row)
        })?;
        let Some((dimensions, nlist, nprobe, train_threshold, state, trained, deletes, count)) =
            row
        else {
            return Ok(None);
        };
        let dimensions = u32::try_from(dimensions)
            .map_err(|_| invalid_ivf_metadata("dimensions", dimensions))?;
        let nlist = positive_i64_to_usize("nlist", nlist)?;
        let nprobe = positive_i64_to_usize("nprobe", nprobe)?;
        let train_threshold = positive_i64_to_usize("train_threshold", train_threshold)?;
        i64_to_usize("trained_size", trained)?;
        i64_to_usize("deletes_since_train", deletes)?;
        Ok(Some(SQLiteIVFMeta {
            dimensions,
            params: SQLiteIVFParams::new(nlist, nprobe, train_threshold),
            state: str_to_state(&state)?,
            vector_count: i64_to_usize("vector_count", count)?,
        }))
    }

    fn load_centroids(&self) -> SQLiteResult<Vec<Vec<f32>>> {
        self.persistent.conn.with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT vector FROM _ivf_centroids
                  WHERE table_name = ?1 AND field = ?2
                  ORDER BY centroid_id",
            )?;
            let rows = stmt
                .query_map(params![self.persistent.table, self.persistent.field], |r| {
                    r.get::<_, Vec<u8>>(0)
                })?;
            let mut out = Vec::new();
            for row in rows {
                let vector = blob_to_vector(&row?)?;
                self.persistent.validate_dimensions_sqlite(&vector)?;
                out.push(vector);
            }
            Ok(out)
        })
    }

    fn load_candidates_for_centroids(
        &self,
        centroids: &[usize],
    ) -> SQLiteResult<Vec<(DocId, Vec<f32>)>> {
        self.persistent.conn.with(|conn| {
            let mut out = Vec::new();
            let mut stmt = conn.prepare(
                "SELECT v.doc_id, v.vector
                   FROM _ivf_assignments a
                   JOIN _vectors v
                     ON v.table_name = a.table_name
                    AND v.field = a.field
                    AND v.doc_id = a.doc_id
                    AND v.vector_ordinal = a.vector_ordinal
                  WHERE a.table_name = ?1
                    AND a.field = ?2
                    AND a.centroid_id = ?3
                  ORDER BY v.doc_id",
            )?;
            for centroid in centroids {
                let centroid = usize_to_i64("centroid_id", *centroid)?;
                let rows = stmt.query_map(
                    params![self.persistent.table, self.persistent.field, centroid],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)),
                )?;
                for row in rows {
                    let (doc_id, blob) = row?;
                    let vector = blob_to_vector(&blob)?;
                    self.persistent.validate_dimensions_sqlite(&vector)?;
                    out.push((decode_doc_id(doc_id)?, vector));
                }
            }
            Ok(out)
        })
    }
}

impl VectorIndex for SQLiteIVFIndex {
    fn dimensions(&self) -> u32 {
        self.persistent.dimensions
    }

    fn index_kind(&self) -> &'static str {
        "sqlite-ivf"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.add_many(doc_id, vec![vector])
    }

    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        let (encoded_doc_id, encoded_vectors) =
            self.persistent.stage_doc_vectors(doc_id, &vectors)?;
        let mut prospective = self.persistent.load_all_with_ordinals()?;
        prospective.retain(|(stored_doc_id, _, _)| *stored_doc_id != doc_id);
        for (ordinal, vector) in vectors.into_iter().enumerate() {
            prospective.push((
                doc_id,
                u32::try_from(ordinal).map_err(|_| {
                    SQLiteError::StorageBackend(
                        "vector ordinal exceeds the u32 index format".into(),
                    )
                })?,
                vector,
            ));
        }
        prospective.sort_by_key(|(doc_id, ordinal, _)| (*doc_id, *ordinal));
        let snapshot = self.metadata_for_entries(&prospective)?;
        let metadata = encode_ivf_metadata(self.params, &snapshot)?;
        self.apply_doc_replacement_atomically(encoded_doc_id, &encoded_vectors, &metadata)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let encoded_doc_id = encode_doc_id(doc_id)?;
        let mut prospective = self.persistent.load_all_with_ordinals()?;
        let previous_len = prospective.len();
        prospective.retain(|(stored_doc_id, _, _)| *stored_doc_id != doc_id);
        if prospective.len() == previous_len {
            return Ok(());
        }
        let snapshot = self.metadata_for_entries(&prospective)?;
        let metadata = encode_ivf_metadata(self.params, &snapshot)?;
        self.apply_doc_delete_atomically(encoded_doc_id, &metadata)
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "DELETE FROM _vectors WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            tx.execute(
                "DELETE FROM _ivf_indexes WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            tx.execute(
                "DELETE FROM _ivf_centroids WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            tx.execute(
                "DELETE FROM _ivf_assignments WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            tx.commit()?;
            Ok(())
        })?;
        Ok(())
    }

    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.persistent.validate_dimensions(query)?;
        if k == 0 {
            return Ok(PostingList::new());
        }
        let Some(meta) = self.ready_meta()? else {
            return self.persistent.search_knn(query, k);
        };
        if meta.state != IVFState::Trained {
            return self.persistent.search_knn(query, k);
        }
        let centroids = self.load_centroids()?;
        if centroids.is_empty() {
            return self.persistent.search_knn(query, k);
        }
        let probe = nearest_centroids_for_raw(query, &centroids, self.params.nprobe);
        let candidates = self.load_candidates_for_centroids(&probe)?;
        if candidates.is_empty() {
            return Ok(PostingList::new());
        }
        Ok(scored_posting_list(query, &candidates, k))
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.persistent.search_threshold(query, threshold)
    }

    fn count(&self) -> StorageBackendResult<usize> {
        self.persistent.count()
    }

    fn initialize(&mut self) -> StorageBackendResult<()> {
        let count = self.persistent.count()?;
        if count >= self.params.train_threshold {
            self.train_metadata()
        } else {
            self.save_untrained_metadata()
        }
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.clone()))
    }
}

fn invalid_ivf_metadata(field: &str, value: i64) -> SQLiteError {
    SQLiteError::StorageBackend(format!("invalid IVF metadata {field}: {value}"))
}

fn i64_to_usize(field: &str, value: i64) -> SQLiteResult<usize> {
    value
        .try_into()
        .map_err(|_| invalid_ivf_metadata(field, value))
}

fn positive_i64_to_usize(field: &str, value: i64) -> SQLiteResult<usize> {
    let value = i64_to_usize(field, value)?;
    if value == 0 {
        Err(SQLiteError::StorageBackend(format!(
            "invalid IVF metadata {field}: expected a positive value"
        )))
    } else {
        Ok(value)
    }
}

fn usize_to_i64(field: &str, value: usize) -> SQLiteResult<i64> {
    value.try_into().map_err(|_| {
        SQLiteError::StorageBackend(format!(
            "IVF metadata {field} does not fit in SQLite INTEGER"
        ))
    })
}

fn encode_ivf_metadata(
    params_value: SQLiteIVFParams,
    snapshot: &IVFMetadataSnapshot,
) -> SQLiteResult<EncodedIVFMetadata> {
    let centroids = snapshot
        .centroids
        .iter()
        .enumerate()
        .map(|(centroid_id, centroid)| {
            Ok((
                usize_to_i64("centroid_id", centroid_id)?,
                vector_to_blob(centroid)?,
            ))
        })
        .collect::<SQLiteResult<Vec<_>>>()?;
    let assignments = snapshot
        .assignments
        .iter()
        .map(|(doc_id, vector_ordinal, centroid_id)| {
            Ok((
                encode_doc_id(*doc_id)?,
                i64::from(*vector_ordinal),
                usize_to_i64("centroid_id", *centroid_id)?,
            ))
        })
        .collect::<SQLiteResult<Vec<_>>>()?;
    Ok(EncodedIVFMetadata {
        nlist: usize_to_i64("nlist", params_value.nlist)?,
        nprobe: usize_to_i64("nprobe", params_value.nprobe)?,
        train_threshold: usize_to_i64("train_threshold", params_value.train_threshold)?,
        state: snapshot.state,
        trained_size: usize_to_i64("trained_size", snapshot.trained_size)?,
        deletes_since_train: usize_to_i64("deletes_since_train", snapshot.deletes_since_train)?,
        vector_count: usize_to_i64("vector_count", snapshot.vector_count)?,
        centroids,
        assignments,
    })
}

fn write_encoded_metadata(
    conn: &rusqlite::Connection,
    persistent: &SQLiteVectorIndex,
    metadata: &EncodedIVFMetadata,
) -> SQLiteResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO _ivf_indexes
            (table_name, field, dimensions, nlist, nprobe, train_threshold,
             state, trained_size, deletes_since_train, vector_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            persistent.table,
            persistent.field,
            i64::from(persistent.dimensions),
            metadata.nlist,
            metadata.nprobe,
            metadata.train_threshold,
            state_to_str(metadata.state),
            metadata.trained_size,
            metadata.deletes_since_train,
            metadata.vector_count,
        ],
    )?;
    conn.execute(
        "DELETE FROM _ivf_centroids WHERE table_name = ?1 AND field = ?2",
        params![persistent.table, persistent.field],
    )?;
    conn.execute(
        "DELETE FROM _ivf_assignments WHERE table_name = ?1 AND field = ?2",
        params![persistent.table, persistent.field],
    )?;
    {
        let mut statement = conn.prepare(
            "INSERT INTO _ivf_centroids
                (table_name, field, centroid_id, vector)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (centroid_id, centroid) in &metadata.centroids {
            statement.execute(params![
                persistent.table,
                persistent.field,
                centroid_id,
                centroid,
            ])?;
        }
    }
    {
        let mut statement = conn.prepare(
            "INSERT INTO _ivf_assignments
                (table_name, field, doc_id, vector_ordinal, centroid_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (doc_id, ordinal, centroid_id) in &metadata.assignments {
            statement.execute(params![
                persistent.table,
                persistent.field,
                doc_id,
                ordinal,
                centroid_id,
            ])?;
        }
    }
    Ok(())
}

fn write_meta_row(
    conn: &rusqlite::Connection,
    persistent: &SQLiteVectorIndex,
    params_value: SQLiteIVFParams,
    state: IVFState,
    trained_size: usize,
    deletes_since_train: usize,
    vector_count: usize,
) -> SQLiteResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO _ivf_indexes
            (table_name, field, dimensions, nlist, nprobe, train_threshold,
             state, trained_size, deletes_since_train, vector_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            persistent.table,
            persistent.field,
            i64::from(persistent.dimensions),
            usize_to_i64("nlist", params_value.nlist)?,
            usize_to_i64("nprobe", params_value.nprobe)?,
            usize_to_i64("train_threshold", params_value.train_threshold)?,
            state_to_str(state),
            usize_to_i64("trained_size", trained_size)?,
            usize_to_i64("deletes_since_train", deletes_since_train)?,
            usize_to_i64("vector_count", vector_count)?,
        ],
    )?;
    Ok(())
}

fn state_to_str(state: IVFState) -> &'static str {
    match state {
        IVFState::Untrained => "untrained",
        IVFState::Trained => "trained",
        IVFState::Stale => "stale",
    }
}

fn str_to_state(value: &str) -> SQLiteResult<IVFState> {
    match value {
        "untrained" => Ok(IVFState::Untrained),
        "trained" => Ok(IVFState::Trained),
        "stale" => Ok(IVFState::Stale),
        other => Err(SQLiteError::StorageBackend(format!(
            "invalid IVF metadata state: {other}"
        ))),
    }
}

fn l2_normalise(v: &mut [f32]) {
    let mag: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag > 1e-12 {
        for x in v.iter_mut() {
            *x /= mag;
        }
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn nearest_centroids_for_raw(vector: &[f32], centroids: &[Vec<f32>], nprobe: usize) -> Vec<usize> {
    let mut q = vector.to_vec();
    l2_normalise(&mut q);
    let mut scored: Vec<(usize, f32)> = centroids
        .iter()
        .enumerate()
        .map(|(i, centroid)| (i, dot(&q, centroid)))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored
        .into_iter()
        .take(nprobe.max(1))
        .map(|(idx, _)| idx)
        .collect()
}

fn scored_posting_list(query: &[f32], entries: &[(DocId, Vec<f32>)], k: usize) -> PostingList {
    let mut best_by_doc: std::collections::BTreeMap<DocId, f32> = std::collections::BTreeMap::new();
    for (doc_id, vector) in entries {
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
    let entries = scored
        .into_iter()
        .map(|(doc_id, sim)| PostingEntry::new(doc_id, Payload::with_score(f64::from(sim))))
        .collect::<Vec<_>>();
    PostingList::from_sorted_unchecked(entries)
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
        idx.add(1, vec![1.0, 0.0, 0.0]).unwrap();
        idx.add(2, vec![0.0, 1.0, 0.0]).unwrap();
        idx.add(3, vec![0.7, 0.7, 0.0]).unwrap();
        let pl = idx.search_knn(&[1.0, 0.0, 0.0], 2).unwrap();
        let docs: Vec<_> = pl.doc_ids().collect();
        assert_eq!(docs, vec![1, 3]);
    }

    #[test]
    fn delete_removes_vector() {
        let mut idx = idx();
        idx.add(1, vec![1.0, 0.0, 0.0]).unwrap();
        idx.delete(1).unwrap();
        assert_eq!(idx.count().unwrap(), 0);
    }

    #[test]
    fn out_of_range_document_id_is_rejected_without_replacing_existing_vectors() {
        let mut idx = idx();
        idx.add(1, vec![1.0, 0.0, 0.0]).unwrap();

        let error = idx.add(u64::MAX, vec![0.0, 1.0, 0.0]).unwrap_err();
        assert!(error.to_string().contains("does not fit in SQLite INTEGER"));
        assert_eq!(idx.count().unwrap(), 1);
        assert_eq!(
            idx.search_knn(&[1.0, 0.0, 0.0], 1)
                .unwrap()
                .doc_ids()
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn negative_persisted_document_id_is_reported_as_corruption() {
        let idx = idx();
        idx.conn
            .with(|conn| {
                conn.execute(
                    "INSERT INTO _vectors
                       (table_name, field, doc_id, vector_ordinal, vector)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        "articles",
                        "embedding",
                        -1_i64,
                        0_i64,
                        vector_to_blob(&[1.0, 0.0, 0.0]).unwrap()
                    ],
                )?;
                Ok(())
            })
            .unwrap();

        let error = idx.search_knn(&[1.0, 0.0, 0.0], 1).unwrap_err();
        assert!(error
            .to_string()
            .contains("invalid negative document id -1"));
    }

    #[test]
    fn non_finite_vectors_queries_and_thresholds_are_rejected() {
        let mut idx = idx();
        assert!(idx.add(1, vec![f32::NAN, 0.0, 0.0]).is_err());
        idx.add(1, vec![1.0, 0.0, 0.0]).unwrap();
        assert!(idx.search_knn(&[f32::INFINITY, 0.0, 0.0], 1).is_err());
        assert!(idx.search_threshold(&[1.0, 0.0, 0.0], f32::NAN).is_err());
    }

    #[test]
    fn round_trip_blob_preserves_bits() {
        let v = vec![0.1f32, -3.5, 12345.678];
        assert_eq!(blob_to_vector(&vector_to_blob(&v).unwrap()).unwrap(), v);
    }

    #[test]
    fn vector_ordinal_count_matches_zero_based_u32_format() {
        validate_vector_ordinal_count(u64::from(u32::MAX) + 1).unwrap();
        let error = validate_vector_ordinal_count(u64::from(u32::MAX) + 2).unwrap_err();
        assert!(error.to_string().contains("u32 index format"));
    }

    #[test]
    fn persisted_vector_ordinal_gaps_are_rejected() {
        let idx = idx();
        idx.conn
            .with(|connection| {
                connection.execute(
                    "INSERT INTO _vectors
                       (table_name, field, doc_id, vector_ordinal, vector)
                     VALUES ('articles', 'embedding', 1, 1, ?1)",
                    [vector_to_blob(&[1.0, 0.0, 0.0])?],
                )?;
                Ok(())
            })
            .unwrap();

        let error = idx.search_knn(&[1.0, 0.0, 0.0], 1).unwrap_err();
        assert!(error.to_string().contains("expected 0, found 1"));
    }

    #[test]
    fn ivf_metadata_conversion_failure_does_not_insert_vectors() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _catalog = Catalog::open(mc.clone()).unwrap();
        let mut idx =
            SQLiteIVFIndex::with_params(mc, "articles", "embedding", 2, usize::MAX, 1, 100);

        let error = idx.add(1, vec![1.0, 0.0]).unwrap_err();
        assert!(error.to_string().contains("nlist"));
        assert_eq!(idx.count().unwrap(), 0);
    }

    #[test]
    fn ivf_metadata_write_failure_rolls_back_vector_replacement() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _catalog = Catalog::open(mc.clone()).unwrap();
        let mut idx =
            SQLiteIVFIndex::with_params(mc.clone(), "articles", "embedding", 2, 4, 2, 100);
        idx.add(1, vec![1.0, 0.0]).unwrap();
        mc.with(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER fail_ivf_metadata
                 BEFORE INSERT ON _ivf_indexes
                 BEGIN
                     SELECT RAISE(ABORT, 'injected IVF metadata failure');
                 END;",
            )?;
            Ok(())
        })
        .unwrap();

        let error = idx.add(2, vec![0.0, 1.0]).unwrap_err();
        assert!(error.to_string().contains("injected IVF metadata failure"));
        assert_eq!(idx.count().unwrap(), 1);
        assert_eq!(
            idx.persistent
                .load_all_with_ordinals()
                .unwrap()
                .into_iter()
                .map(|(doc_id, _, _)| doc_id)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn sqlite_ivf_persists_metadata_and_reopens() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _cat = Catalog::open(mc.clone()).unwrap();
        {
            let mut idx =
                SQLiteIVFIndex::with_params(mc.clone(), "articles", "embedding", 3, 3, 2, 3);
            idx.add(1, vec![1.0, 0.0, 0.0]).unwrap();
            idx.add(2, vec![0.0, 1.0, 0.0]).unwrap();
            idx.add(3, vec![0.8, 0.2, 0.0]).unwrap();
        }

        let centroid_count: i64 = mc
            .with(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM _ivf_centroids
                      WHERE table_name = 'articles' AND field = 'embedding'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert!(centroid_count > 0);

        let idx = SQLiteIVFIndex::with_params(mc, "articles", "embedding", 3, 3, 2, 3);
        assert_eq!(idx.index_kind(), "sqlite-ivf");
        assert_eq!(idx.count().unwrap(), 3);
        let pl = idx.search_knn(&[1.0, 0.0, 0.0], 2).unwrap();
        let docs: Vec<_> = pl.doc_ids().collect();
        assert_eq!(docs, vec![1, 3]);
    }

    #[test]
    fn sqlite_ivf_uses_persisted_assignments_without_retraining() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _cat = Catalog::open(mc.clone()).unwrap();
        let mut raw = SQLiteVectorIndex::new(mc.clone(), "articles", "embedding", 2);
        raw.add(1, vec![1.0, 0.0]).unwrap();
        raw.add(2, vec![0.0, 1.0]).unwrap();
        mc.with(|conn| {
            conn.execute(
                "INSERT INTO _ivf_indexes
                    (table_name, field, dimensions, nlist, nprobe, train_threshold,
                     state, trained_size, deletes_since_train, vector_count)
                 VALUES ('articles', 'embedding', 2, 2, 1, 2, 'trained', 2, 0, 2)",
                [],
            )?;
            conn.execute(
                "INSERT INTO _ivf_centroids (table_name, field, centroid_id, vector)
                 VALUES ('articles', 'embedding', 0, ?1)",
                params![vector_to_blob(&[1.0, 0.0]).unwrap()],
            )?;
            conn.execute(
                "INSERT INTO _ivf_centroids (table_name, field, centroid_id, vector)
                 VALUES ('articles', 'embedding', 1, ?1)",
                params![vector_to_blob(&[0.0, 1.0]).unwrap()],
            )?;
            // Deliberately inverted assignments. A rebuild from raw
            // vectors would put doc 1 in centroid 0; metadata reuse keeps
            // doc 2 as the only candidate for a [1, 0] query with nprobe=1.
            conn.execute(
                "INSERT INTO _ivf_assignments (table_name, field, doc_id, vector_ordinal, centroid_id)
                 VALUES ('articles', 'embedding', 1, 0, 1)",
                [],
            )?;
            conn.execute(
                "INSERT INTO _ivf_assignments (table_name, field, doc_id, vector_ordinal, centroid_id)
                 VALUES ('articles', 'embedding', 2, 0, 0)",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let idx = SQLiteIVFIndex::with_params(mc, "articles", "embedding", 2, 2, 1, 2);
        let pl = idx.search_knn(&[1.0, 0.0], 1).unwrap();
        let docs: Vec<_> = pl.doc_ids().collect();
        assert_eq!(docs, vec![2]);
    }
}
