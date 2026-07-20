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
use crate::sqlite::connection::{ManagedConnection, Result as SQLiteResult};
use crate::vector_index::{cosine_similarity, select_top_k_scored, VectorIndex};

const STALE_FRACTION: f64 = 0.20;

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
                out.push((
                    doc_id as DocId,
                    ordinal.try_into().unwrap_or(0),
                    blob_to_vector(&blob),
                ));
            }
            Ok(out)
        })
    }

    fn load_doc_with_ordinals(&self, doc_id: DocId) -> SQLiteResult<Vec<(u32, Vec<f32>)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare(
                "SELECT vector_ordinal, vector FROM _vectors
                 WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3
                 ORDER BY vector_ordinal",
            )?;
            let rows = stmt.query_map(params![self.table, self.field, doc_id as i64], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (ordinal, blob) = row?;
                out.push((ordinal.try_into().unwrap_or(0), blob_to_vector(&blob)));
            }
            Ok(out)
        })
    }

    fn doc_vector_count(&self, doc_id: DocId) -> usize {
        self.conn
            .with(|conn| {
                let n: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM _vectors
                      WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
                    params![self.table, self.field, doc_id as i64],
                    |r| r.get(0),
                )?;
                Ok(n as usize)
            })
            .unwrap_or(0)
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

    fn index_kind(&self) -> &'static str {
        "sqlite-bruteforce"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) {
        debug_assert_eq!(
            vector.len() as u32,
            self.dimensions,
            "vector dimension mismatch"
        );
        self.add_many(doc_id, vec![vector]);
    }

    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) {
        for vector in &vectors {
            debug_assert_eq!(
                vector.len() as u32,
                self.dimensions,
                "vector dimension mismatch"
            );
        }
        let _ = self.conn.with(|c| {
            c.execute(
                "DELETE FROM _vectors
                 WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
                params![self.table, self.field, doc_id as i64],
            )?;
            let mut stmt = c.prepare(
                "INSERT INTO _vectors (table_name, field, doc_id, vector_ordinal, vector)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (ordinal, vector) in vectors.iter().enumerate() {
                if vector.len() as u32 != self.dimensions {
                    continue;
                }
                stmt.execute(params![
                    self.table,
                    self.field,
                    doc_id as i64,
                    ordinal as i64,
                    vector_to_blob(vector),
                ])?;
            }
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
        PostingList::from_sorted_unchecked(entries)
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> PostingList {
        let entries = self.load_all().unwrap_or_default();
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
    trained_size: usize,
    deletes_since_train: usize,
    vector_count: usize,
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
        let mut idx = Self {
            persistent: SQLiteVectorIndex::new(conn, table, field, dimensions),
            params: SQLiteIVFParams::new(100, 10, 256),
        };
        if let Some(meta) = idx.load_meta().unwrap_or(None) {
            idx.params = meta.params;
        }
        idx.bootstrap_metadata();
        idx
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
        let idx = Self {
            persistent: SQLiteVectorIndex::new(conn, table, field, dimensions),
            params: SQLiteIVFParams::new(nlist, nprobe, train_threshold),
        };
        idx.bootstrap_metadata();
        idx
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
        conn.with(|conn| {
            conn.execute(
                "DELETE FROM _ivf_indexes WHERE table_name = ?1 AND field = ?2",
                params![table, field],
            )?;
            conn.execute(
                "DELETE FROM _ivf_centroids WHERE table_name = ?1 AND field = ?2",
                params![table, field],
            )?;
            conn.execute(
                "DELETE FROM _ivf_assignments WHERE table_name = ?1 AND field = ?2",
                params![table, field],
            )?;
            Ok(())
        })
    }

    fn bootstrap_metadata(&self) {
        match self.load_meta().unwrap_or(None) {
            Some(meta)
                if meta.dimensions == self.persistent.dimensions && meta.params == self.params => {}
            Some(_) | None => {
                if self.persistent.count() >= self.params.train_threshold {
                    self.train_metadata();
                } else {
                    self.save_untrained_metadata();
                }
            }
        }
    }

    fn ready_meta(&self) -> Option<SQLiteIVFMeta> {
        let meta = self.load_meta().unwrap_or(None);
        match meta {
            Some(meta) if meta.state == IVFState::Stale => {
                self.train_metadata();
                self.load_meta().unwrap_or(None)
            }
            Some(meta)
                if meta.state == IVFState::Untrained
                    && self.persistent.count() >= self.params.train_threshold =>
            {
                self.train_metadata();
                self.load_meta().unwrap_or(None)
            }
            Some(meta)
                if meta.state == IVFState::Trained
                    && (meta.dimensions != self.persistent.dimensions
                        || meta.params != self.params
                        || meta.vector_count != self.persistent.count()) =>
            {
                self.train_metadata();
                self.load_meta().unwrap_or(None)
            }
            Some(meta) => Some(meta),
            None => {
                self.bootstrap_metadata();
                self.load_meta().unwrap_or(None)
            }
        }
    }

    fn train_metadata(&self) {
        let entries = self.persistent.load_all_with_ordinals().unwrap_or_default();
        if entries.len() < self.params.train_threshold {
            self.clear_metadata_lists();
            self.save_meta_row(IVFState::Untrained, 0, 0, entries.len());
            return;
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
            );
        }
        ivf.train();
        self.save_trained_metadata(&ivf.metadata_snapshot());
    }

    fn save_untrained_metadata(&self) {
        self.clear_metadata_lists();
        self.save_meta_row(IVFState::Untrained, 0, 0, self.persistent.count());
    }

    fn save_trained_metadata(&self, snapshot: &IVFMetadataSnapshot) {
        let _ = self.persistent.conn.with_mut(|conn| {
            let tx = conn.savepoint()?;
            tx.execute(
                "INSERT OR REPLACE INTO _ivf_indexes
                    (table_name, field, dimensions, nlist, nprobe, train_threshold,
                     state, trained_size, deletes_since_train, vector_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    self.persistent.table,
                    self.persistent.field,
                    i64::from(self.persistent.dimensions),
                    self.params.nlist as i64,
                    self.params.nprobe as i64,
                    self.params.train_threshold as i64,
                    state_to_str(snapshot.state),
                    snapshot.trained_size as i64,
                    snapshot.deletes_since_train as i64,
                    snapshot.vector_count as i64,
                ],
            )?;
            tx.execute(
                "DELETE FROM _ivf_centroids WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            tx.execute(
                "DELETE FROM _ivf_assignments WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO _ivf_centroids
                        (table_name, field, centroid_id, vector)
                     VALUES (?1, ?2, ?3, ?4)",
                )?;
                for (centroid_id, centroid) in snapshot.centroids.iter().enumerate() {
                    stmt.execute(params![
                        self.persistent.table,
                        self.persistent.field,
                        centroid_id as i64,
                        vector_to_blob(centroid),
                    ])?;
                }
            }
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO _ivf_assignments
                        (table_name, field, doc_id, vector_ordinal, centroid_id)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )?;
                for (doc_id, vector_ordinal, centroid_id) in &snapshot.assignments {
                    stmt.execute(params![
                        self.persistent.table,
                        self.persistent.field,
                        *doc_id as i64,
                        i64::from(*vector_ordinal),
                        *centroid_id as i64,
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        });
    }

    fn save_meta_row(
        &self,
        state: IVFState,
        trained_size: usize,
        deletes_since_train: usize,
        vector_count: usize,
    ) {
        let _ = self.persistent.conn.with(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO _ivf_indexes
                    (table_name, field, dimensions, nlist, nprobe, train_threshold,
                     state, trained_size, deletes_since_train, vector_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    self.persistent.table,
                    self.persistent.field,
                    i64::from(self.persistent.dimensions),
                    self.params.nlist as i64,
                    self.params.nprobe as i64,
                    self.params.train_threshold as i64,
                    state_to_str(state),
                    trained_size as i64,
                    deletes_since_train as i64,
                    vector_count as i64,
                ],
            )?;
            Ok(())
        });
    }

    fn clear_metadata_lists(&self) {
        let _ = self.persistent.conn.with(|conn| {
            conn.execute(
                "DELETE FROM _ivf_centroids WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            conn.execute(
                "DELETE FROM _ivf_assignments WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            Ok(())
        });
    }

    fn clear_metadata(&self) {
        let _ = self.persistent.conn.with(|conn| {
            conn.execute(
                "DELETE FROM _ivf_indexes WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            conn.execute(
                "DELETE FROM _ivf_centroids WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            conn.execute(
                "DELETE FROM _ivf_assignments WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
            )?;
            Ok(())
        });
    }

    fn load_meta(&self) -> SQLiteResult<Option<SQLiteIVFMeta>> {
        self.persistent.conn.with(|conn| {
            conn.query_row(
                "SELECT dimensions, nlist, nprobe, train_threshold, state,
                        trained_size, deletes_since_train, vector_count
                   FROM _ivf_indexes
                  WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field],
                |r| {
                    let dimensions = r.get::<_, i64>(0)?.try_into().unwrap_or(0);
                    let nlist = i64_to_usize(r.get::<_, i64>(1)?);
                    let nprobe = i64_to_usize(r.get::<_, i64>(2)?);
                    let train_threshold = i64_to_usize(r.get::<_, i64>(3)?);
                    let state = str_to_state(&r.get::<_, String>(4)?);
                    Ok(SQLiteIVFMeta {
                        dimensions,
                        params: SQLiteIVFParams::new(nlist, nprobe, train_threshold),
                        state,
                        trained_size: i64_to_usize(r.get::<_, i64>(5)?),
                        deletes_since_train: i64_to_usize(r.get::<_, i64>(6)?),
                        vector_count: i64_to_usize(r.get::<_, i64>(7)?),
                    })
                },
            )
            .optional()
            .map_err(Into::into)
        })
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
                out.push(blob_to_vector(&row?));
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
                let rows = stmt.query_map(
                    params![
                        self.persistent.table,
                        self.persistent.field,
                        *centroid as i64
                    ],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)),
                )?;
                for row in rows {
                    let (doc_id, blob) = row?;
                    out.push((doc_id as DocId, blob_to_vector(&blob)));
                }
            }
            Ok(out)
        })
    }

    fn assign_existing_centroid(
        &self,
        doc_id: DocId,
        vector_ordinal: u32,
        vector: &[f32],
        meta: &SQLiteIVFMeta,
    ) {
        if !matches!(meta.state, IVFState::Trained | IVFState::Stale) {
            return;
        }
        let centroids = self.load_centroids().unwrap_or_default();
        if centroids.is_empty() {
            self.train_metadata();
            return;
        }
        let centroid = nearest_centroid_for_raw(vector, &centroids);
        let vector_count = self.persistent.count() as i64;
        let _ = self.persistent.conn.with(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO _ivf_assignments
                    (table_name, field, doc_id, vector_ordinal, centroid_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    self.persistent.table,
                    self.persistent.field,
                    doc_id as i64,
                    i64::from(vector_ordinal),
                    centroid as i64,
                ],
            )?;
            conn.execute(
                "UPDATE _ivf_indexes
                    SET vector_count = ?3
                  WHERE table_name = ?1 AND field = ?2",
                params![self.persistent.table, self.persistent.field, vector_count],
            )?;
            Ok(())
        });
    }

    fn clear_assignments_for_doc(&self, doc_id: DocId) {
        let _ = self.persistent.conn.with(|conn| {
            conn.execute(
                "DELETE FROM _ivf_assignments
                  WHERE table_name = ?1 AND field = ?2 AND doc_id = ?3",
                params![self.persistent.table, self.persistent.field, doc_id as i64],
            )?;
            Ok(())
        });
    }

    fn remove_assignment_and_mark_stale(&self, doc_id: DocId, removed_vectors: usize) {
        let meta = self.load_meta().unwrap_or(None);
        self.clear_assignments_for_doc(doc_id);
        if let Some(meta) = meta {
            let deletes = meta
                .deletes_since_train
                .saturating_add(removed_vectors.max(1));
            let mut state = meta.state;
            if meta.trained_size > 0
                && (deletes as f64) / (meta.trained_size as f64) > STALE_FRACTION
            {
                state = IVFState::Stale;
            }
            self.save_meta_row(state, meta.trained_size, deletes, self.persistent.count());
        }
    }
}

impl VectorIndex for SQLiteIVFIndex {
    fn dimensions(&self) -> u32 {
        self.persistent.dimensions
    }

    fn index_kind(&self) -> &'static str {
        "sqlite-ivf"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) {
        self.add_many(doc_id, vec![vector]);
    }

    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) {
        self.persistent.add_many(doc_id, vectors);
        self.clear_assignments_for_doc(doc_id);
        let count = self.persistent.count();
        match self.load_meta().unwrap_or(None) {
            Some(meta) if count >= self.params.train_threshold => {
                if meta.state == IVFState::Untrained
                    || meta.dimensions != self.persistent.dimensions
                    || meta.params != self.params
                {
                    self.train_metadata();
                } else {
                    let doc_vectors = self
                        .persistent
                        .load_doc_with_ordinals(doc_id)
                        .unwrap_or_default();
                    for (ordinal, vector) in doc_vectors {
                        self.assign_existing_centroid(doc_id, ordinal, &vector, &meta);
                    }
                }
            }
            Some(_meta) if count < self.params.train_threshold => self.save_untrained_metadata(),
            Some(meta) => {
                self.save_meta_row(
                    meta.state,
                    meta.trained_size,
                    meta.deletes_since_train,
                    count,
                );
            }
            None if count >= self.params.train_threshold => self.train_metadata(),
            None => self.save_untrained_metadata(),
        }
    }

    fn delete(&mut self, doc_id: DocId) {
        let existing_count = self.persistent.doc_vector_count(doc_id);
        self.persistent.delete(doc_id);
        if existing_count > 0 {
            self.remove_assignment_and_mark_stale(doc_id, existing_count);
        }
    }

    fn clear(&mut self) {
        self.persistent.clear();
        self.clear_metadata();
    }

    fn search_knn(&self, query: &[f32], k: usize) -> PostingList {
        if query.len() as u32 != self.persistent.dimensions || k == 0 {
            return PostingList::new();
        }
        let Some(meta) = self.ready_meta() else {
            return self.persistent.search_knn(query, k);
        };
        if meta.state != IVFState::Trained {
            return self.persistent.search_knn(query, k);
        }
        let centroids = self.load_centroids().unwrap_or_default();
        if centroids.is_empty() {
            return self.persistent.search_knn(query, k);
        }
        let probe = nearest_centroids_for_raw(query, &centroids, self.params.nprobe);
        let candidates = self
            .load_candidates_for_centroids(&probe)
            .unwrap_or_default();
        if candidates.is_empty() {
            return PostingList::new();
        }
        scored_posting_list(query, &candidates, k)
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> PostingList {
        self.persistent.search_threshold(query, threshold)
    }

    fn count(&self) -> usize {
        self.persistent.count()
    }

    fn snapshot(&self) -> Arc<dyn VectorIndex> {
        Arc::new(self.clone())
    }
}

fn i64_to_usize(value: i64) -> usize {
    value.try_into().unwrap_or(0)
}

fn state_to_str(state: IVFState) -> &'static str {
    match state {
        IVFState::Untrained => "untrained",
        IVFState::Trained => "trained",
        IVFState::Stale => "stale",
    }
}

fn str_to_state(value: &str) -> IVFState {
    match value {
        "trained" => IVFState::Trained,
        "stale" => IVFState::Stale,
        _ => IVFState::Untrained,
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

fn nearest_centroid_for_raw(vector: &[f32], centroids: &[Vec<f32>]) -> usize {
    let mut q = vector.to_vec();
    l2_normalise(&mut q);
    let mut best_idx = 0usize;
    let mut best_sim = f32::NEG_INFINITY;
    for (i, c) in centroids.iter().enumerate() {
        let sim = dot(&q, c);
        if sim > best_sim {
            best_sim = sim;
            best_idx = i;
        }
    }
    best_idx
}

fn nearest_centroids_for_raw(vector: &[f32], centroids: &[Vec<f32>], nprobe: usize) -> Vec<usize> {
    let mut q = vector.to_vec();
    l2_normalise(&mut q);
    let mut scored: Vec<(usize, f32)> = centroids
        .iter()
        .enumerate()
        .map(|(i, centroid)| (i, dot(&q, centroid)))
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
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

    #[test]
    fn sqlite_ivf_persists_metadata_and_reopens() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _cat = Catalog::open(mc.clone()).unwrap();
        {
            let mut idx =
                SQLiteIVFIndex::with_params(mc.clone(), "articles", "embedding", 3, 3, 2, 3);
            idx.add(1, vec![1.0, 0.0, 0.0]);
            idx.add(2, vec![0.0, 1.0, 0.0]);
            idx.add(3, vec![0.8, 0.2, 0.0]);
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
        assert_eq!(idx.count(), 3);
        let pl = idx.search_knn(&[1.0, 0.0, 0.0], 2);
        let docs: Vec<_> = pl.doc_ids().collect();
        assert_eq!(docs, vec![1, 3]);
    }

    #[test]
    fn sqlite_ivf_uses_persisted_assignments_without_retraining() {
        let mc = ManagedConnection::open_in_memory().unwrap();
        let _cat = Catalog::open(mc.clone()).unwrap();
        let mut raw = SQLiteVectorIndex::new(mc.clone(), "articles", "embedding", 2);
        raw.add(1, vec![1.0, 0.0]);
        raw.add(2, vec![0.0, 1.0]);
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
                params![vector_to_blob(&[1.0, 0.0])],
            )?;
            conn.execute(
                "INSERT INTO _ivf_centroids (table_name, field, centroid_id, vector)
                 VALUES ('articles', 'embedding', 1, ?1)",
                params![vector_to_blob(&[0.0, 1.0])],
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
        let pl = idx.search_knn(&[1.0, 0.0], 1);
        let docs: Vec<_> = pl.doc_ids().collect();
        assert_eq!(docs, vec![2]);
    }
}
