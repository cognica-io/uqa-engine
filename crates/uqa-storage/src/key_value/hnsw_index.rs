//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! HNSW readers and evaluated mutations use one logical Key/Value visibility boundary.

use std::sync::Arc;
use uqa_core::{DocId, PostingList};

use super::codec::encode_value;
use super::codec::{other_error, vector_field_prefix};
use super::hnsw_persistence::{self, metadata_from_graph, PersistedHNSWNode};
use super::index_keys::{hnsw_metadata_key, hnsw_node_key, hnsw_node_prefix};
use super::{KeyValueBatch, KeyValueRead, KeyValueStore, KeyValueVectorIndex};
use crate::hnsw_index::{HNSWIndex, HNSWMutation, HNSWPersistenceDelta};
use crate::vector_index::{HNSWIndexParams, VectorIndex};
use crate::{StorageBackendError, StorageBackendResult};

use super::index_view::{read_view, IndexState, IndexView};

pub struct KeyValueHNSWIndex {
    store: Arc<dyn KeyValueStore>,
    raw: KeyValueVectorIndex,
    table: String,
    field: String,
    dimensions: u32,
    params: HNSWIndexParams,
    require_persisted: bool,
    view: IndexView<HNSWIndex>,
}

impl KeyValueHNSWIndex {
    pub fn create(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        params: HNSWIndexParams,
    ) -> StorageBackendResult<Self> {
        let index = Self::new(store, table.into(), field.into(), dimensions, params, false)?;
        index.read_graph()?;
        Ok(index)
    }

    pub fn restore(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        params: HNSWIndexParams,
    ) -> StorageBackendResult<Self> {
        let index = Self::new(store, table.into(), field.into(), dimensions, params, true)?;
        index.read_graph()?;
        Ok(index)
    }

    fn new(
        store: Arc<dyn KeyValueStore>,
        table: String,
        field: String,
        dimensions: u32,
        params: HNSWIndexParams,
        require_persisted: bool,
    ) -> StorageBackendResult<Self> {
        Ok(Self {
            raw: KeyValueVectorIndex::new(Arc::clone(&store), &table, &field, dimensions),
            store,
            table,
            field,
            dimensions,
            params: params.validate()?,
            require_persisted,
            view: IndexView::new(!require_persisted),
        })
    }

    fn graph_at(&self, read: &dyn KeyValueRead) -> StorageBackendResult<IndexState<HNSWIndex>> {
        self.view.load(
            read,
            &[
                &hnsw_metadata_key(&self.table, &self.field)?,
                &hnsw_node_prefix(&self.table, &self.field)?,
                &vector_field_prefix(&self.table, &self.field)?,
            ],
            |creating| {
                let revision = hnsw_persistence::load_revision(read, &self.table, &self.field)?;
                let graph = if creating || (revision.is_none() && !self.require_persisted) {
                    self.build_from_canonical(read)?
                } else {
                    hnsw_persistence::restore_graph(
                        read,
                        &self.raw,
                        &self.table,
                        &self.field,
                        self.dimensions,
                        self.params,
                    )?
                    .0
                };
                Ok((graph, revision))
            },
        )
    }

    fn read_graph(&self) -> StorageBackendResult<IndexState<HNSWIndex>> {
        read_view(self.store.as_ref(), |read| self.graph_at(read))
    }

    fn mutate_graph(
        &self,
        mutation: HNSWMutation<'_>,
        canonical: impl FnOnce(&mut dyn KeyValueBatch) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.view.evaluate(self.store.as_ref(), |read, batch| {
            let cached = self.graph_at(read)?;
            let delta = cached.value.prepare_delta(mutation, read.control())?;
            canonical(batch)?;
            let preview = !cached.definition_candidate
                && cached.revision.is_some()
                && !matches!(mutation, HNSWMutation::Clear);
            if preview {
                batch.hnsw_mutation(&hnsw_metadata_key(&self.table, &self.field)?, mutation)?;
            }
            // Only a later reader publishes the graph with its actual committed/private identity.
            self.stage_delta(batch, &delta, next_revision(cached.revision)?, preview)
        })
    }

    fn rebuild_graph(&self) -> StorageBackendResult<()> {
        self.view.evaluate(self.store.as_ref(), |read, batch| {
            let revision = hnsw_persistence::load_revision(read, &self.table, &self.field)?;
            if revision.is_none() && self.require_persisted {
                return Err(other_error(format!(
                    "missing persisted HNSW metadata for {}.{}",
                    self.table, self.field
                )));
            }
            let vectors = self.raw.load_all_from(read)?;
            let delta = HNSWIndex::prepare_canonical(
                self.dimensions,
                self.params,
                &vectors,
                read.control(),
            )?;
            self.stage_delta(batch, &delta, next_revision(revision)?, false)
        })
    }

    fn build_from_canonical(&self, read: &dyn KeyValueRead) -> StorageBackendResult<HNSWIndex> {
        let entries = self.raw.load_all_from(read)?;
        let mut graph = HNSWIndex::with_params(self.dimensions, self.params)?;
        for vectors in entries.chunk_by(|a, b| a.0 == b.0) {
            read.control().check()?;
            graph.add_many(
                vectors[0].0,
                vectors
                    .iter()
                    .map(|(_, _, vector)| vector.clone())
                    .collect(),
            )?;
        }
        Ok(graph)
    }

    fn stage_delta(
        &self,
        batch: &mut dyn KeyValueBatch,
        delta: &HNSWPersistenceDelta,
        revision: u64,
        preview: bool,
    ) -> StorageBackendResult<()> {
        let metadata = hnsw_metadata_key(&self.table, &self.field)?;
        let value = encode_value(&metadata_from_graph(
            self.dimensions,
            self.params,
            delta.meta,
            revision,
        )?)?;
        if preview {
            batch.preview_hnsw_record(&metadata, Some(&value))?;
        } else {
            batch.fence_hnsw_prefix(&metadata)?;
            batch.put(&metadata, &value)?;
        }
        if delta.full_rewrite {
            let prefix = hnsw_node_prefix(&self.table, &self.field)?;
            if preview {
                batch.preview_hnsw_prefix(&prefix)?;
            } else {
                batch.delete_prefix(&prefix)?;
            }
        }
        for node in &delta.nodes {
            let key = hnsw_node_key(&self.table, &self.field, node.node_id)?;
            let value = encode_value(&PersistedHNSWNode::try_from(node)?)?;
            if preview {
                batch.preview_hnsw_record(&key, Some(&value))?;
            } else {
                batch.put(&key, &value)?;
            }
        }
        Ok(())
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
        self.add_many(doc_id, vec![vector])
    }
    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        self.mutate_graph(
            HNSWMutation::Replace {
                document: doc_id,
                vectors: &vectors,
            },
            |batch| self.raw.stage_replace(batch, doc_id, &vectors),
        )
    }
    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.mutate_graph(HNSWMutation::Delete(doc_id), |batch| {
            self.raw.stage_replace(batch, doc_id, &[])
        })
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        self.mutate_graph(HNSWMutation::Clear, |batch| self.raw.stage_clear(batch))
    }
    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.read_graph()?.value.search_knn(query, k)
    }
    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.read_graph()?.value.search_threshold(query, threshold)
    }
    fn count(&self) -> StorageBackendResult<usize> {
        self.read_graph()?.value.count()
    }
    fn initialize(&mut self) -> StorageBackendResult<()> {
        self.rebuild_graph()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(self.read_graph()?.snapshot)
    }
}

fn next_revision(revision: Option<u64>) -> StorageBackendResult<u64> {
    revision
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| StorageBackendError::Other("HNSW metadata revision space exhausted".into()))
}
