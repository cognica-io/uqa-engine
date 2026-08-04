//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transactional HNSW graph persistence over the logical Key/Value backend.

use std::sync::Arc;

use parking_lot::Mutex;
use uqa_core::{DocId, PostingList};

use super::codec::other_error;
use super::hnsw_persistence;
use super::{KeyValueStore, KeyValueVectorIndex};
use crate::hnsw_index::{HNSWIndex, HNSWPersistenceDelta};
use crate::vector_index::{HNSWIndexParams, VectorIndex};
use crate::{StorageBackendError, StorageBackendResult};

struct CachedHNSW {
    graph: HNSWIndex,
    revision: Option<u64>,
}

pub struct KeyValueHNSWIndex {
    store: Arc<dyn KeyValueStore>,
    raw: KeyValueVectorIndex,
    table: String,
    field: String,
    dimensions: u32,
    params: HNSWIndexParams,
    cached: Mutex<CachedHNSW>,
}

impl KeyValueHNSWIndex {
    pub fn create(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        params: HNSWIndexParams,
    ) -> StorageBackendResult<Self> {
        let params = params.validate()?;
        let table = table.into();
        let field = field.into();
        let raw = KeyValueVectorIndex::new(Arc::clone(&store), &table, &field, dimensions);
        let graph = build_from_canonical(&raw, dimensions, params)?;
        Ok(Self {
            store,
            raw,
            table,
            field,
            dimensions,
            params,
            cached: Mutex::new(CachedHNSW {
                graph,
                revision: None,
            }),
        })
    }

    pub fn restore(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        params: HNSWIndexParams,
    ) -> StorageBackendResult<Self> {
        let params = params.validate()?;
        let table = table.into();
        let field = field.into();
        let raw = KeyValueVectorIndex::new(Arc::clone(&store), &table, &field, dimensions);
        let (graph, revision) = hnsw_persistence::restore_graph(
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
            cached: Mutex::new(CachedHNSW {
                graph,
                revision: Some(revision),
            }),
        })
    }

    fn replace_document(&self, doc_id: DocId, vectors: &[Vec<f32>]) -> StorageBackendResult<()> {
        let mut cached = self.cached.lock();
        self.verify_revision(cached.revision)?;
        let mut graph = cached.graph.clone();
        graph.add_many(doc_id, vectors.to_vec())?;
        let delta = graph.take_persistence_delta();
        let revision = next_revision(cached.revision)?;
        let mut batch = self.store.batch();
        self.raw.stage_replace(batch.as_mut(), doc_id, vectors)?;
        self.stage_delta(batch.as_mut(), &delta, revision)?;
        batch.commit()?;
        *cached = CachedHNSW {
            graph,
            revision: Some(revision),
        };
        Ok(())
    }

    fn delete_document(&self, doc_id: DocId) -> StorageBackendResult<()> {
        let mut cached = self.cached.lock();
        self.verify_revision(cached.revision)?;
        let mut graph = cached.graph.clone();
        graph.delete(doc_id)?;
        let delta = graph.take_persistence_delta();
        let revision = next_revision(cached.revision)?;
        let mut batch = self.store.batch();
        self.raw.stage_replace(batch.as_mut(), doc_id, &[])?;
        self.stage_delta(batch.as_mut(), &delta, revision)?;
        batch.commit()?;
        *cached = CachedHNSW {
            graph,
            revision: Some(revision),
        };
        Ok(())
    }

    fn clear_graph(&self) -> StorageBackendResult<()> {
        let mut cached = self.cached.lock();
        self.verify_revision(cached.revision)?;
        let mut graph = cached.graph.clone();
        graph.clear()?;
        let delta = graph.take_persistence_delta();
        let revision = next_revision(cached.revision)?;
        let mut batch = self.store.batch();
        self.raw.stage_clear(batch.as_mut())?;
        self.stage_delta(batch.as_mut(), &delta, revision)?;
        batch.commit()?;
        *cached = CachedHNSW {
            graph,
            revision: Some(revision),
        };
        Ok(())
    }

    fn rebuild_graph(&self) -> StorageBackendResult<()> {
        let mut cached = self.cached.lock();
        self.verify_revision(cached.revision)?;
        let mut graph = build_from_canonical(&self.raw, self.dimensions, self.params)?;
        let delta = graph.take_persistence_delta();
        let revision = next_revision(cached.revision)?;
        let mut batch = self.store.batch();
        self.stage_delta(batch.as_mut(), &delta, revision)?;
        batch.commit()?;
        *cached = CachedHNSW {
            graph,
            revision: Some(revision),
        };
        Ok(())
    }

    fn verify_revision(&self, expected: Option<u64>) -> StorageBackendResult<()> {
        let Some(expected) = expected else {
            return Ok(());
        };
        let actual =
            hnsw_persistence::load_revision(self.store.as_ref(), &self.table, &self.field)?
                .ok_or_else(|| {
                    other_error(format!(
                        "missing persisted HNSW metadata for {}.{}",
                        self.table, self.field
                    ))
                })?;
        if actual != expected {
            return Err(other_error(format!(
                "concurrent HNSW metadata change for {}.{}: expected revision {expected}, found {actual}",
                self.table, self.field
            )));
        }
        Ok(())
    }

    fn stage_delta(
        &self,
        batch: &mut dyn super::KeyValueBatch,
        delta: &HNSWPersistenceDelta,
        revision: u64,
    ) -> StorageBackendResult<()> {
        hnsw_persistence::stage_delta(
            batch,
            &self.table,
            &self.field,
            self.dimensions,
            self.params,
            delta,
            revision,
        )
    }
}

impl VectorIndex for KeyValueHNSWIndex {
    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    fn index_kind(&self) -> &'static str {
        "hnsw"
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
        self.clear_graph()
    }

    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.cached.lock().graph.search_knn(query, k)
    }

    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.cached.lock().graph.search_threshold(query, threshold)
    }

    fn count(&self) -> StorageBackendResult<usize> {
        self.cached.lock().graph.count()
    }

    fn initialize(&mut self) -> StorageBackendResult<()> {
        self.rebuild_graph()
    }

    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.cached.lock().graph.clone()))
    }
}

fn build_from_canonical(
    raw: &KeyValueVectorIndex,
    dimensions: u32,
    params: HNSWIndexParams,
) -> StorageBackendResult<HNSWIndex> {
    let mut graph = HNSWIndex::with_params(dimensions, params)?;
    for (doc_id, vectors) in raw.load_by_document()? {
        graph.add_many(doc_id, vectors)?;
    }
    Ok(graph)
}

fn next_revision(revision: Option<u64>) -> StorageBackendResult<u64> {
    revision
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| StorageBackendError::Other("HNSW metadata revision space exhausted".into()))
}
