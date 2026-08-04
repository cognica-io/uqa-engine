//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transactional IVF physical state over the logical Key/Value backend.

use std::sync::Arc;

use parking_lot::Mutex;
use uqa_core::{DocId, PostingList};

use super::codec::other_error;
use super::index_keys::{
    hnsw_metadata_key, hnsw_node_prefix, ivf_assignment_prefix, ivf_centroid_prefix,
    ivf_metadata_key,
};
use super::ivf_persistence;
use super::{KeyValueStore, KeyValueVectorIndex};
use crate::ivf_index::{IVFIndex, IVFMetadataSnapshot, IVFState};
use crate::vector_index::{IVFIndexParams, VectorIndex};
use crate::{StorageBackendError, StorageBackendResult};

struct CachedIVF {
    index: IVFIndex,
    revision: Option<u64>,
}

pub struct KeyValueIVFIndex {
    store: Arc<dyn KeyValueStore>,
    raw: KeyValueVectorIndex,
    table: String,
    field: String,
    dimensions: u32,
    params: IVFIndexParams,
    cached: Mutex<CachedIVF>,
}

impl KeyValueIVFIndex {
    pub fn create(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        params: IVFIndexParams,
    ) -> StorageBackendResult<Self> {
        let params = params.validate()?;
        let table = table.into();
        let field = field.into();
        let raw = KeyValueVectorIndex::new(Arc::clone(&store), &table, &field, dimensions);
        let index = build_from_canonical(&raw, dimensions, params)?;
        Ok(Self {
            store,
            raw,
            table,
            field,
            dimensions,
            params,
            cached: Mutex::new(CachedIVF {
                index,
                revision: None,
            }),
        })
    }

    pub fn restore(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        params: IVFIndexParams,
    ) -> StorageBackendResult<Self> {
        let params = params.validate()?;
        let table = table.into();
        let field = field.into();
        let raw = KeyValueVectorIndex::new(Arc::clone(&store), &table, &field, dimensions);
        let (index, revision) = ivf_persistence::restore_state(
            store.as_ref(),
            &raw,
            &table,
            &field,
            dimensions,
            params,
        )?;
        Ok(Self {
            store,
            raw,
            table,
            field,
            dimensions,
            params,
            cached: Mutex::new(CachedIVF {
                index,
                revision: Some(revision),
            }),
        })
    }

    pub(super) fn drop_metadata(
        store: &dyn KeyValueStore,
        table: &str,
        field: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = store.batch();
        batch.delete(&ivf_metadata_key(table, field)?)?;
        batch.delete_prefix(&ivf_centroid_prefix(table, field)?)?;
        batch.delete_prefix(&ivf_assignment_prefix(table, field)?)?;
        batch.delete(&hnsw_metadata_key(table, field)?)?;
        batch.delete_prefix(&hnsw_node_prefix(table, field)?)?;
        batch.commit()
    }

    fn replace_document(&self, doc_id: DocId, vectors: &[Vec<f32>]) -> StorageBackendResult<()> {
        let mut cached = self.cached.lock();
        self.verify_revision(cached.revision)?;
        let before = cached.index.metadata_snapshot();
        let mut staged = cached.index.detached_clone();
        staged.add_many(doc_id, vectors.to_vec())?;
        train_if_stale(&staged)?;
        let after = staged.metadata_snapshot();
        let full_rewrite = cached.revision.is_none() || before.centroids != after.centroids;
        let revision = next_revision(cached.revision)?;
        let mut batch = self.store.batch();
        self.raw.stage_replace(batch.as_mut(), doc_id, vectors)?;
        self.stage_snapshot(batch.as_mut(), &after, revision, full_rewrite, Some(doc_id))?;
        batch.commit()?;
        *cached = CachedIVF {
            index: staged,
            revision: Some(revision),
        };
        Ok(())
    }

    fn delete_document(&self, doc_id: DocId) -> StorageBackendResult<()> {
        let mut cached = self.cached.lock();
        self.verify_revision(cached.revision)?;
        let before = cached.index.metadata_snapshot();
        let mut staged = cached.index.detached_clone();
        staged.delete(doc_id)?;
        train_if_stale(&staged)?;
        let after = staged.metadata_snapshot();
        let full_rewrite = cached.revision.is_none() || before.centroids != after.centroids;
        let revision = next_revision(cached.revision)?;
        let mut batch = self.store.batch();
        self.raw.stage_replace(batch.as_mut(), doc_id, &[])?;
        self.stage_snapshot(batch.as_mut(), &after, revision, full_rewrite, Some(doc_id))?;
        batch.commit()?;
        *cached = CachedIVF {
            index: staged,
            revision: Some(revision),
        };
        Ok(())
    }

    fn replace_all(&self, clear_vectors: bool, train: bool) -> StorageBackendResult<()> {
        let mut cached = self.cached.lock();
        self.verify_revision(cached.revision)?;
        let mut staged = cached.index.detached_clone();
        if clear_vectors {
            staged.clear()?;
        } else if train {
            staged.initialize()?;
        }
        let snapshot = staged.metadata_snapshot();
        let revision = next_revision(cached.revision)?;
        let mut batch = self.store.batch();
        if clear_vectors {
            self.raw.stage_clear(batch.as_mut())?;
        }
        self.stage_snapshot(batch.as_mut(), &snapshot, revision, true, None)?;
        batch.commit()?;
        *cached = CachedIVF {
            index: staged,
            revision: Some(revision),
        };
        Ok(())
    }

    fn verify_revision(&self, expected: Option<u64>) -> StorageBackendResult<()> {
        let Some(expected) = expected else {
            return Ok(());
        };
        let actual = ivf_persistence::load_revision(self.store.as_ref(), &self.table, &self.field)?
            .ok_or_else(|| {
                other_error(format!(
                    "missing persisted IVF metadata for {}.{}",
                    self.table, self.field
                ))
            })?;
        if actual != expected {
            return Err(other_error(format!(
                "concurrent IVF metadata change for {}.{}: expected revision {expected}, found {actual}",
                self.table, self.field
            )));
        }
        Ok(())
    }

    fn stage_snapshot(
        &self,
        batch: &mut dyn super::KeyValueBatch,
        snapshot: &IVFMetadataSnapshot,
        revision: u64,
        full_rewrite: bool,
        changed_doc: Option<DocId>,
    ) -> StorageBackendResult<()> {
        ivf_persistence::stage_snapshot(
            batch,
            &self.table,
            &self.field,
            self.dimensions,
            self.params,
            snapshot,
            revision,
            full_rewrite,
            changed_doc,
        )
    }
}

impl VectorIndex for KeyValueIVFIndex {
    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    fn index_kind(&self) -> &'static str {
        "ivf"
    }

    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.replace_document(doc_id, &[vector])
    }

    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        self.replace_document(doc_id, &vectors)
    }

    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.delete_document(doc_id)
    }

    fn clear(&mut self) -> StorageBackendResult<()> {
        self.replace_all(true, false)
    }

    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        let cached = self.cached.lock();
        if cached.index.state() != IVFState::Stale {
            return cached.index.search_knn(query, k);
        }
        let staged = cached.index.detached_clone();
        drop(cached);
        staged.train()?;
        staged.search_knn(query, k)
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.cached.lock().index.search_threshold(query, threshold)
    }

    fn count(&self) -> StorageBackendResult<usize> {
        self.cached.lock().index.count()
    }

    fn initialize(&mut self) -> StorageBackendResult<()> {
        self.replace_all(false, true)
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        let staged = self.cached.lock().index.detached_clone();
        train_if_stale(&staged)?;
        Ok(Arc::new(staged))
    }
}

fn build_from_canonical(
    raw: &KeyValueVectorIndex,
    dimensions: u32,
    params: IVFIndexParams,
) -> StorageBackendResult<IVFIndex> {
    let mut index = IVFIndex::with_params(
        dimensions,
        params.nlist,
        params.nprobe,
        params.train_threshold,
    );
    for (doc_id, vectors) in raw.load_by_document()? {
        index.add_many(doc_id, vectors)?;
    }
    Ok(index)
}

fn next_revision(revision: Option<u64>) -> StorageBackendResult<u64> {
    revision
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| StorageBackendError::Other("IVF metadata revision space exhausted".into()))
}

fn train_if_stale(index: &IVFIndex) -> StorageBackendResult<()> {
    if index.state() == IVFState::Stale {
        index.train()?;
    }
    Ok(())
}
