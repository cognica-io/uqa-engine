//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector-index adapter over an ordered key/value store.

use std::collections::BTreeMap;

use super::codec::{
    blob_to_vector, other_error, read_str, read_u64, usize_to_u64, validate_vector_ordinal_count,
    vector_doc_prefix, vector_field_prefix, vector_key, vector_to_blob,
};
use super::{
    cosine_similarity, validate_vector_values, Arc, DocId, KeyValueBatch, KeyValueStore, Payload,
    PostingEntry, PostingList, StorageBackendResult, VectorIndex,
};

/// Brute-force vector index implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueVectorIndex {
    store: Arc<dyn KeyValueStore>,
    table: String,
    field: String,
    dimensions: u32,
}

impl KeyValueVectorIndex {
    pub fn new(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
    ) -> Self {
        Self {
            store,
            table: table.into(),
            field: field.into(),
            dimensions,
        }
    }

    pub(super) fn load_all_with_ordinals(
        &self,
    ) -> StorageBackendResult<Vec<(DocId, u32, Vec<f32>)>> {
        let mut vectors = Vec::new();
        let mut current_doc = None;
        let mut expected_ordinal = 0_u32;
        for (key, value) in self
            .store
            .scan_prefix(&vector_field_prefix(&self.table, &self.field)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _field = read_str(&key, &mut offset)?;
            let doc_id = read_u64(&key, &mut offset)?;
            let ordinal = read_u64(&key, &mut offset)?;
            let ordinal = u32::try_from(ordinal)
                .map_err(|_| other_error("persisted vector ordinal exceeds u32 index format"))?;
            if offset != key.len() {
                return Err(other_error("persisted vector key has trailing bytes"));
            }
            if current_doc != Some(doc_id) {
                current_doc = Some(doc_id);
                expected_ordinal = 0;
            }
            if ordinal != expected_ordinal {
                return Err(other_error(format!(
                    "invalid persisted vector ordinal sequence for document {doc_id}: expected {expected_ordinal}, found {ordinal}"
                )));
            }
            expected_ordinal = expected_ordinal
                .checked_add(1)
                .ok_or_else(|| other_error("persisted vector ordinal sequence overflow"))?;
            let vector = blob_to_vector(&value)?;
            self.validate_dimensions(&vector)?;
            vectors.push((doc_id, ordinal, vector));
        }
        Ok(vectors)
    }

    pub(super) fn load_all(&self) -> StorageBackendResult<Vec<(DocId, Vec<f32>)>> {
        Ok(self
            .load_all_with_ordinals()?
            .into_iter()
            .map(|(doc_id, _, vector)| (doc_id, vector))
            .collect())
    }

    pub(super) fn load_by_document(&self) -> StorageBackendResult<BTreeMap<DocId, Vec<Vec<f32>>>> {
        let mut grouped = BTreeMap::<DocId, Vec<Vec<f32>>>::new();
        for (doc_id, ordinal, vector) in self.load_all_with_ordinals()? {
            let vectors = grouped.entry(doc_id).or_default();
            if usize::try_from(ordinal).ok() != Some(vectors.len()) {
                return Err(other_error(format!(
                    "invalid canonical vector ordinal for document {doc_id}: expected {}, found {ordinal}",
                    vectors.len()
                )));
            }
            vectors.push(vector);
        }
        Ok(grouped)
    }

    pub(super) fn stage_replace(
        &self,
        batch: &mut dyn KeyValueBatch,
        doc_id: DocId,
        vectors: &[Vec<f32>],
    ) -> StorageBackendResult<()> {
        for vector in vectors {
            self.validate_dimensions(vector)?;
        }
        validate_vector_ordinal_count(usize_to_u64(vectors.len(), "vector count")?)?;
        batch.delete_prefix(&vector_doc_prefix(&self.table, &self.field, doc_id)?)?;
        for (ordinal, vector) in vectors.iter().enumerate() {
            let ordinal = u32::try_from(ordinal)
                .map_err(|_| other_error("vector ordinal exceeds u32 index format"))?;
            batch.put(
                &vector_key(&self.table, &self.field, doc_id, ordinal)?,
                &vector_to_blob(vector)?,
            )?;
        }
        Ok(())
    }

    pub(super) fn stage_clear(&self, batch: &mut dyn KeyValueBatch) -> StorageBackendResult<()> {
        batch.delete_prefix(&vector_field_prefix(&self.table, &self.field)?)
    }

    fn validate_dimensions(&self, vector: &[f32]) -> StorageBackendResult<()> {
        validate_vector_values(self.dimensions, vector).map_err(|error| {
            other_error(format!(
                "invalid vector for {}.{}: {error}",
                self.table, self.field
            ))
        })
    }
}

impl VectorIndex for KeyValueVectorIndex {
    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    fn index_kind(&self) -> &'static str {
        "keyvalue-bruteforce"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.add_many(doc_id, vec![vector])
    }

    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        self.stage_replace(batch.as_mut(), doc_id, &vectors)?;
        batch.commit()
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete_prefix(&vector_doc_prefix(&self.table, &self.field, doc_id)?)?;
        batch.commit()
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        self.stage_clear(batch.as_mut())?;
        batch.commit()
    }

    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.validate_dimensions(query)?;
        if k == 0 {
            return Ok(PostingList::new());
        }
        let entries = self.load_all()?;
        let mut best_by_doc = BTreeMap::<DocId, f32>::new();
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
        let mut scored = best_by_doc.into_iter().collect::<Vec<_>>();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        scored.truncate(k);
        scored.sort_by_key(|(doc_id, _)| *doc_id);
        Ok(PostingList::from_sorted_unchecked(
            scored
                .into_iter()
                .map(|(doc_id, sim)| PostingEntry::new(doc_id, Payload::with_score(f64::from(sim))))
                .collect(),
        ))
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.validate_dimensions(query)?;
        if !threshold.is_finite() {
            return Err(other_error(format!(
                "vector similarity threshold must be finite, got {threshold}"
            )));
        }
        let mut best_by_doc = BTreeMap::<DocId, f32>::new();
        for (doc_id, vector) in self.load_all()? {
            let sim = cosine_similarity(query, &vector);
            if sim >= threshold {
                best_by_doc
                    .entry(doc_id)
                    .and_modify(|best| {
                        if sim > *best {
                            *best = sim;
                        }
                    })
                    .or_insert(sim);
            }
        }
        let mut entries = best_by_doc
            .into_iter()
            .map(|(doc_id, sim)| PostingEntry::new(doc_id, Payload::with_score(f64::from(sim))))
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.doc_id);
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    fn count(&self) -> StorageBackendResult<usize> {
        Ok(self
            .store
            .scan_prefix(&vector_field_prefix(&self.table, &self.field)?)?
            .len())
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.clone()))
    }
}
