//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector-index adapter over an ordered key/value store.

mod guards;
pub(in crate::key_value) mod origin;
mod read;
pub use origin::{
    DiskANNCanonicalVectorVisitor, KeyValueDiskANNCanonical, KeyValueDiskANNHandle,
    RetainedDiskANNCanonical,
};

use crate::vector_index::RetainedVectorIndex;

use super::index_view::read_view;

use super::codec::{
    other_error, usize_to_u64, validate_vector_ordinal_count, vector_doc_prefix,
    vector_field_prefix, vector_key, vector_to_blob,
};
use super::{
    validate_vector_values, Arc, DocId, KeyValueBatch, KeyValueStore, PostingList,
    StorageBackendResult, VectorIndex,
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

    fn read_snapshot(&self) -> StorageBackendResult<RetainedVectorIndex> {
        read_view(self.store.as_ref(), |read| {
            RetainedVectorIndex::from_entries(
                self.load_all_from(read)?,
                self.dimensions,
                "keyvalue-bruteforce",
                read.control(),
            )
        })
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
        self.coordinate_field(batch, false)?;
        batch.delete(&origin::key(&self.table, &self.field, doc_id)?)?;
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
        self.coordinate_field(batch, true)?;
        batch.delete_prefix(&origin::prefix(&self.table, &self.field)?)?;
        batch.delete_prefix(&origin::journal::prefix(&self.table, &self.field)?)?;
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
    fn contains_document(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        read_view(self.store.as_ref(), |read| {
            read.control().check()?;
            // The existing key encoder can hold both old and growing buffers; retain their allowance until the key is dropped.
            let bytes = self
                .table
                .len()
                .checked_add(self.field.len())
                .and_then(|length| length.checked_add(25))
                .and_then(|length| length.checked_mul(3))
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
            let _key_memory = read.control().memory().reserve(bytes)?;
            let key = vector_key(&self.table, &self.field, doc_id, 0)?;
            let mut found = false;
            read.visit_keys_after(&key, None, 1, read.control(), &mut |candidate| {
                found = candidate == key;
                Ok(())
            })?;
            Ok(found)
        })
    }

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
        self.stage_replace(batch.as_mut(), doc_id, &[])?;
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
        self.read_snapshot()?.search_knn(query, k)
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.validate_dimensions(query)?;
        if !threshold.is_finite() {
            return Err(other_error(format!(
                "vector similarity threshold must be finite, got {threshold}"
            )));
        }
        self.read_snapshot()?.search_threshold(query, threshold)
    }

    fn count(&self) -> StorageBackendResult<usize> {
        self.read_snapshot()?.count()
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.read_snapshot()?))
    }
}
