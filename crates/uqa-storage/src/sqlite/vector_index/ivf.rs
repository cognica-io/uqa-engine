//! SQLite-backed IVF lifecycle, mutation, and search.

use super::{
    blob_to_vector, decode_doc_id, encode_doc_id, encode_ivf_metadata, i64_to_usize,
    invalid_ivf_metadata, nearest_centroids_for_raw, params, positive_i64_to_usize,
    scored_posting_list, str_to_state, usize_to_i64, validate_persisted_ordinal_sequence,
    write_encoded_metadata, write_meta_row, Arc, DocId, EncodedIVFMetadata, IVFIndex,
    IVFMetadataSnapshot, IVFState, ManagedConnection, OptionalExtension, PostingList, SQLiteError,
    SQLiteIVFMeta, SQLiteIVFParams, SQLiteResult, SQLiteVectorIndex, StorageBackendResult,
    VectorIndex,
};

#[derive(Clone)]
pub struct SQLiteIVFIndex {
    pub(super) persistent: SQLiteVectorIndex,
    pub(super) params: SQLiteIVFParams,
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
