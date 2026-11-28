//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Approximate top-k and exact threshold search retain each reader's workspace allowance.

use std::cmp::Reverse;
use uqa_core::{memory::BudgetedBinaryHeap, DocId, PostingList};

use super::state::{IVFData, IVFState, StoredVector};
use crate::read_control::StorageReadControl;
use crate::vector_index::{
    cosine_similarity_with_norms, maximum_tensor_score,
    query::{check, nearest_normalized_centroids, postings_from_scores, TopK, VectorQueryBuffer},
    validate_vector_values, vector_norm,
};
use crate::{StorageBackendError, StorageBackendResult};

impl IVFData {
    pub(super) fn search_top_k(
        &self,
        query: &[f32],
        normalized: &[f32],
        norm: f32,
        k: usize,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<PostingList> {
        let fallback;
        let control = if let Some(control) = control {
            control
        } else {
            fallback = StorageReadControl::with_limit(usize::MAX);
            &fallback
        };
        let mut top = TopK::new(k, control);
        let mut current: Option<(DocId, f32)> = None;
        let mut score = |vector: &StoredVector| -> StorageBackendResult<()> {
            control.check()?;
            let similarity =
                cosine_similarity_with_norms(query, &vector.raw_vector, norm, vector.norm);
            if let Some((_, best)) = current.as_mut().filter(|(id, _)| *id == vector.doc_id) {
                *best = maximum_tensor_score(*best, similarity);
            } else if let Some((document, best)) = current.replace((vector.doc_id, similarity)) {
                top.offer_raw(document, best)?;
            }
            Ok(())
        };
        if self.state() == IVFState::Untrained || self.centroids.is_empty() {
            for vector in self.vectors.values() {
                score(vector)?;
            }
        } else {
            let probes = nearest_normalized_centroids(
                normalized,
                &self.centroids,
                self.nprobe(),
                Some(control),
            )?;
            self.visit_probed_vectors(&probes, control, score)?;
        }
        if let Some((document, best)) = current {
            top.offer_raw(document, best)?;
        }
        top.finish(control)
    }

    // Each centroid list is sorted by (document, ordinal). Merge by document, keeping probe order within each document for the tensor maximum.
    fn visit_probed_vectors(
        &self,
        probes: &[usize],
        control: &StorageReadControl,
        mut visit: impl FnMut(&StoredVector) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        let mut heads = BudgetedBinaryHeap::new(control.memory());
        for (rank, &centroid) in probes.iter().enumerate() {
            control.check()?;
            if let Some(&(document, _)) = self
                .inverted_lists
                .get(centroid)
                .and_then(|list| list.first())
            {
                heads.push(Reverse((document, rank, 0_usize)))?;
            }
        }
        while let Some(Reverse((_, rank, position))) = heads.pop() {
            control.check()?;
            let list = &self.inverted_lists[probes[rank]];
            if let Some(vector) = self.vectors.get(&list[position]) {
                visit(vector)?;
            }
            let next = position + 1;
            if let Some(&(document, _)) = list.get(next) {
                heads.push(Reverse((document, rank, next)))?;
            }
        }
        Ok(())
    }

    pub(super) fn search_above_threshold(
        &self,
        query: &[f32],
        threshold: f32,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<PostingList> {
        check(control)?;
        validate_vector_values(self.dimensions, query)?;
        if !threshold.is_finite() {
            return Err(StorageBackendError::Other(format!(
                "vector similarity threshold must be finite, got {threshold}"
            )));
        }
        let norm = vector_norm(query);
        let vectors = &self.vectors;
        let mut scored = VectorQueryBuffer::new(control);
        for vector in vectors.values() {
            check(control)?;
            let similarity =
                cosine_similarity_with_norms(query, &vector.raw_vector, norm, vector.norm);
            if similarity >= threshold {
                scored.push((vector.doc_id, similarity))?;
            }
        }
        postings_from_scores(scored, None, control)
    }
}
