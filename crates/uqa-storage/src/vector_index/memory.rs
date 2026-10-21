//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutable exact vectors share an immutable root with read snapshots.

use super::{
    memory_state::MemoryVectorState,
    query::{self, TopK, VectorQueryBuffer},
    VectorIndex,
};
use crate::{read_control::StorageReadControl, StorageBackendResult};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};
use uqa_core::{DocId, PostingList};

#[derive(Debug)]
pub struct MemoryVectorIndex {
    pub(super) dimensions: u32,
    pub(super) state: Arc<MemoryVectorState>,
    // The legacy full-map API keeps its concrete Rust type. Query and snapshot paths borrow the owned map directly and never construct this export.
    exported_vectors: OnceLock<BTreeMap<DocId, Vec<Vec<f32>>>>,
}

impl Clone for MemoryVectorIndex {
    fn clone(&self) -> Self {
        Self {
            dimensions: self.dimensions,
            state: Arc::new((*self.state).clone()),
            exported_vectors: OnceLock::new(),
        }
    }
}

impl MemoryVectorIndex {
    pub fn new(dimensions: u32) -> Self {
        Self {
            dimensions,
            state: Arc::new(MemoryVectorState::default()),
            exported_vectors: OnceLock::new(),
        }
    }

    /// Export the complete canonical map, retaining its existing concrete Rust type. The owned export is formed on the first explicit request and invalidated by mutation; query and snapshot APIs do not materialize it.
    pub fn vectors(&self) -> &BTreeMap<DocId, Vec<Vec<f32>>> {
        self.exported_vectors.get_or_init(|| {
            self.state
                .vectors
                .iter()
                .map(|(&document, vectors)| (document, vectors.clone()))
                .collect()
        })
    }

    pub(super) fn shared_snapshot(&self) -> Self {
        Self {
            dimensions: self.dimensions,
            state: Arc::clone(&self.state),
            exported_vectors: OnceLock::new(),
        }
    }

    fn replace(&mut self, document: DocId, vectors: Vec<Vec<f32>>) {
        self.exported_vectors.take();
        Arc::make_mut(&mut self.state).replace(document, vectors);
    }

    fn visit_scores(
        &self,
        query: &[f32],
        control: Option<&StorageReadControl>,
        mut visit: impl FnMut(DocId, f32) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        for (&document, vectors) in &self.state.vectors {
            query::check(control)?;
            let mut best = None;
            for vector in vectors {
                let score =
                    super::cosine_similarity_controlled(query, vector, || query::check(control))?;
                if best.is_none_or(|best| score.total_cmp(&best).is_gt()) {
                    best = Some(score);
                }
            }
            if let Some(score) = best {
                visit(document, score)?;
            }
        }
        query::check(control)
    }

    fn knn(
        &self,
        query: &[f32],
        k: usize,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<PostingList> {
        query::check(control)?;
        super::validate_vector_values_controlled(self.dimensions, query, control)?;
        if k == 0 || self.state.vectors.is_empty() {
            return Ok(PostingList::new());
        }
        // The ordinary API keeps its unrestricted caller-owned result boundary; controlled readers use their original allowance for all selection and output buffers.
        let ordinary;
        let control = if let Some(control) = control {
            control
        } else {
            ordinary = StorageReadControl::with_limit(usize::MAX);
            &ordinary
        };
        let mut selected = TopK::new(k, control);
        self.visit_scores(query, Some(control), |document, score| {
            selected.offer_raw(document, score)
        })?;
        selected.finish(control)
    }

    fn threshold(
        &self,
        query: &[f32],
        threshold: f32,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<PostingList> {
        query::check(control)?;
        super::validate_vector_values_controlled(self.dimensions, query, control)?;
        super::validate_threshold(threshold)?;
        let mut scores = VectorQueryBuffer::new(control);
        self.visit_scores(query, control, |document, score| {
            if score >= threshold {
                scores.push((document, score))?;
            }
            Ok(())
        })?;
        query::postings_from_unique_scores(scores, None, control)
    }
}

impl VectorIndex for MemoryVectorIndex {
    fn contains_document(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        Ok(self.state.vectors.contains_key(&doc_id))
    }
    fn dimensions(&self) -> u32 {
        self.dimensions
    }
    fn index_kind(&self) -> &'static str {
        "memory-bruteforce"
    }
    fn add(&mut self, doc_id: DocId, vector: Vec<f32>) -> StorageBackendResult<()> {
        super::validate_vector_values(self.dimensions, &vector)?;
        self.replace(doc_id, vec![vector]);
        Ok(())
    }
    fn add_many(&mut self, doc_id: DocId, vectors: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        for vector in &vectors {
            super::validate_vector_values(self.dimensions, vector)?;
        }
        self.replace(doc_id, vectors);
        Ok(())
    }
    fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
        if self.state.vectors.contains_key(&doc_id) {
            self.replace(doc_id, Vec::new());
        }
        Ok(())
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        self.exported_vectors.take();
        self.state = Arc::new(MemoryVectorState::default());
        Ok(())
    }
    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.knn(query, k, None)
    }
    fn search_knn_with_control(
        &self,
        query: &[f32],
        k: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        self.knn(query, k, Some(control))
    }
    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.threshold(query, threshold, None)
    }
    fn search_threshold_with_control(
        &self,
        query: &[f32],
        threshold: f32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        self.threshold(query, threshold, Some(control))
    }
    fn count(&self) -> StorageBackendResult<usize> {
        super::checked_vector_count(self.state.vector_count)
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(crate::ReadOnlySnapshot::new(Arc::new(
            self.shared_snapshot(),
        ))))
    }
    fn snapshot_with_control(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        super::memory_snapshot::capture(self, control)
    }
    fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn VectorIndex>> {
        Ok(Box::new(self.shared_snapshot()))
    }
}
