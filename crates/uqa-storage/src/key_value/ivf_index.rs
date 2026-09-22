//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! IVF state and canonical values share one logical Key/Value visibility boundary.

use std::sync::Arc;

use uqa_core::{memory::Budgeted, DocId, PostingList};

use super::codec::{other_error, vector_field_prefix};
use super::index_keys::{
    hnsw_metadata_key, hnsw_node_prefix, ivf_assignment_prefix, ivf_centroid_prefix,
    ivf_metadata_key,
};
use super::index_view::{read_view, IndexState, IndexView};
use super::ivf_persistence;
use super::{KeyValueBatch, KeyValueRead, KeyValueStore, KeyValueVectorIndex};
use crate::ivf_index::{IVFIndex, IVFMetadataSnapshot, IVFMutation, IVFState};
use crate::vector_index::{IVFIndexParams, VectorIndex};
use crate::{ReadOnlySnapshot, StorageBackendError, StorageBackendResult};

pub struct KeyValueIVFIndex {
    store: Arc<dyn KeyValueStore>,
    raw: KeyValueVectorIndex,
    table: String,
    field: String,
    dimensions: u32,
    params: IVFIndexParams,
    require_persisted: bool,
    view: IndexView<IVFIndex>,
}

impl KeyValueIVFIndex {
    pub fn create(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        params: IVFIndexParams,
    ) -> StorageBackendResult<Self> {
        let index = Self::new(store, table.into(), field.into(), dimensions, params, false)?;
        index.read_index()?;
        Ok(index)
    }

    pub fn restore(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
        params: IVFIndexParams,
    ) -> StorageBackendResult<Self> {
        let index = Self::new(store, table.into(), field.into(), dimensions, params, true)?;
        index.read_index()?;
        Ok(index)
    }

    fn new(
        store: Arc<dyn KeyValueStore>,
        table: String,
        field: String,
        dimensions: u32,
        params: IVFIndexParams,
        require_persisted: bool,
    ) -> StorageBackendResult<Self> {
        Ok(Self {
            raw: KeyValueVectorIndex::new(store.clone(), &table, &field, dimensions),
            store,
            table,
            field,
            dimensions,
            params: params.validate()?,
            require_persisted,
            view: IndexView::new(!require_persisted),
        })
    }

    pub(super) fn drop_metadata(
        store: &dyn KeyValueStore,
        table: &str,
        field: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = store.batch();
        batch.fence_ivf_prefix(&ivf_metadata_key(table, field)?)?;
        batch.fence_hnsw_prefix(&hnsw_metadata_key(table, field)?)?;
        batch.delete(&ivf_metadata_key(table, field)?)?;
        batch.delete_prefix(&ivf_centroid_prefix(table, field)?)?;
        batch.delete_prefix(&ivf_assignment_prefix(table, field)?)?;
        batch.delete(&hnsw_metadata_key(table, field)?)?;
        batch.delete_prefix(&hnsw_node_prefix(table, field)?)?;
        batch.commit()
    }

    fn index_at(&self, read: &dyn KeyValueRead) -> StorageBackendResult<IndexState<IVFIndex>> {
        self.view.load(
            read,
            &[
                &ivf_metadata_key(&self.table, &self.field)?,
                &ivf_centroid_prefix(&self.table, &self.field)?,
                &ivf_assignment_prefix(&self.table, &self.field)?,
                &vector_field_prefix(&self.table, &self.field)?,
            ],
            |creating| {
                let revision = ivf_persistence::load_revision(read, &self.table, &self.field)?;
                let index = if creating || (revision.is_none() && !self.require_persisted) {
                    self.build_from_canonical(read)?
                } else {
                    ivf_persistence::restore_state(
                        read,
                        &self.raw,
                        &self.table,
                        &self.field,
                        self.dimensions,
                        self.params,
                    )?
                    .0
                };
                Ok((index, revision))
            },
        )
    }

    fn read_index(&self) -> StorageBackendResult<IndexState<IVFIndex>> {
        read_view(self.store.as_ref(), |read| self.index_at(read))
    }

    fn mutate(
        &self,
        mutation: IVFMutation<'_>,
        canonical: impl FnOnce(&mut dyn KeyValueBatch) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.view.evaluate(self.store.as_ref(), |read, batch| {
            let cached = self.index_at(read)?;
            let after = cached.value.prepare_metadata(mutation, read.control())?;
            let changed_doc = match mutation {
                IVFMutation::Replace { document, .. } | IVFMutation::Delete(document) => {
                    Some(document)
                }
                IVFMutation::Clear | IVFMutation::Train => None,
            };
            let full_rewrite = changed_doc.is_none()
                || cached.definition_candidate
                || cached.revision.is_none()
                || !cached.value.centroids_match(&after);
            canonical(batch)?;
            let preview =
                !cached.definition_candidate && cached.revision.is_some() && changed_doc.is_some();
            if preview {
                batch.ivf_mutation(&ivf_metadata_key(&self.table, &self.field)?, mutation)?;
            }
            self.stage_snapshot(
                batch,
                &after,
                next_revision(cached.revision)?,
                full_rewrite,
                changed_doc,
                preview,
            )
        })
    }

    fn build_from_canonical(
        &self,
        read: &dyn KeyValueRead,
    ) -> StorageBackendResult<Budgeted<IVFIndex>> {
        let entries = self.raw.load_all_from(read)?;
        IVFIndex::from_canonical_controlled(self.dimensions, self.params, &entries, read.control())
    }

    fn rebuild(&self) -> StorageBackendResult<()> {
        self.view.evaluate(self.store.as_ref(), |read, batch| {
            let revision = ivf_persistence::load_revision(read, &self.table, &self.field)?;
            if revision.is_none() && self.require_persisted {
                return Err(other_error(format!(
                    "missing persisted IVF metadata for {}.{}",
                    self.table, self.field
                )));
            }
            let vectors = self.raw.load_all_from(read)?;
            let candidate = IVFIndex::prepare_canonical(
                self.dimensions,
                self.params,
                &vectors,
                read.control(),
            )?;
            self.stage_snapshot(
                batch,
                &candidate,
                next_revision(revision)?,
                true,
                None,
                false,
            )
        })
    }

    fn stage_snapshot(
        &self,
        batch: &mut dyn KeyValueBatch,
        snapshot: &IVFMetadataSnapshot,
        revision: u64,
        full_rewrite: bool,
        changed_doc: Option<DocId>,
        preview: bool,
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
            preview,
        )
    }
}

impl VectorIndex for KeyValueIVFIndex {
    fn contains_document(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        self.raw.contains_document(doc_id)
    }

    fn dimensions(&self) -> u32 {
        self.dimensions
    }
    fn index_kind(&self) -> &'static str {
        "ivf"
    }
    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        self.add_many(doc_id, vec![vector])
    }
    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        self.mutate(
            IVFMutation::Replace {
                document: doc_id,
                vectors: &vectors,
            },
            |batch| self.raw.stage_replace(batch, doc_id, &vectors),
        )
    }
    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        self.mutate(IVFMutation::Delete(doc_id), |batch| {
            self.raw.stage_replace(batch, doc_id, &[])
        })
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        self.mutate(IVFMutation::Clear, |batch| self.raw.stage_clear(batch))
    }
    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.snapshot()?.search_knn(query, k)
    }
    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.read_index()?.value.search_threshold(query, threshold)
    }
    fn count(&self) -> StorageBackendResult<usize> {
        self.read_index()?.value.count()
    }
    fn initialize(&mut self) -> StorageBackendResult<()> {
        self.rebuild()
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        read_view(self.store.as_ref(), |read| {
            let cached = self.index_at(read)?;
            if cached.value.state() != IVFState::Stale {
                return Ok(cached.snapshot);
            }
            let candidate = cached.value.trained_snapshot_controlled(read.control())?;
            ReadOnlySnapshot::from_budgeted(candidate)?.snapshot()
        })
    }
}

fn next_revision(revision: Option<u64>) -> StorageBackendResult<u64> {
    revision
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| StorageBackendError::Other("IVF metadata revision space exhausted".into()))
}
