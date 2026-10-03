//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical HNSW queries retain invocation-owned workspace without changing graph traversal.

use uqa_core::{memory::BudgetedBinaryHeap, DocId, PostingList};

use super::{prepare::Control, search::Candidate, store::Map, HNSWIndex};
use crate::{
    vector_index::{
        cosine_similarity_with_norms,
        query::{check, normalized_query, postings_from_unique_scores, VectorQueryBuffer},
        validate_vector_values, vector_norm,
    },
    StorageBackendError, StorageBackendResult,
};

impl HNSWIndex {
    pub(super) fn search_top_k(
        &self,
        query: &[f32],
        k: usize,
        control: Control<'_>,
    ) -> StorageBackendResult<PostingList> {
        check(control)?;
        validate_vector_values(self.dimensions, query)?;
        if k == 0 || self.active.is_empty() {
            return Ok(PostingList::new());
        }
        let (normalized, norm) = normalized_query(query, control)?;
        let mut ef = self.params.ef_search.max(k).min(self.nodes.len());
        let mut scored = VectorQueryBuffer::<(DocId, f32)>::new(control);
        loop {
            scored.clear();
            let memory = control.map_or(&self.memory, |control| control.memory());
            let mut best = Map::<u64>::new(memory, memory.limit() / 32);
            let candidates = self.query_candidates(&normalized, ef, control)?;
            for candidate in candidates.iter() {
                check(control)?;
                let candidate = candidate?;
                let Some(node) = self.node(candidate.node_id)? else {
                    continue;
                };
                if !node.deleted {
                    let score =
                        cosine_similarity_with_norms(query, &node.raw_vector, norm, node.norm);
                    let key = u128::from(node.doc_id);
                    let score = best
                        .get(key)?
                        .map_or(score, |value| f32::from_bits(*value as u32).max(score));
                    best.insert(key, u64::from(score.to_bits()), control)?;
                }
            }
            drop(candidates);
            if best.len() >= k || ef >= self.nodes.len() {
                // Reduce complete per-document maxima before selecting K: finite inputs can overflow to NaN, whose later finite tensor member changes its maximum.
                let mut top = BudgetedBinaryHeap::<Candidate>::new(memory);
                for value in best.iter() {
                    check(control)?;
                    let (document, score) = value?;
                    // Negation reverses total float order, including signed zero and NaN payloads. The largest candidate is the worst score, with largest document identity breaking a tie.
                    let candidate = Candidate {
                        node_id: document as DocId,
                        distance: -f32::from_bits(*score as u32),
                    };
                    if top.len() < k {
                        top.push(candidate)?;
                    } else if top.peek().is_some_and(|worst| candidate < *worst) {
                        top.pop();
                        top.push(candidate)?;
                    }
                }
                drop(best);
                let top = top.into_vec();
                for candidate in top.iter() {
                    check(control)?;
                    scored.push((candidate.node_id, -candidate.distance))?;
                }
                drop(top);
                return postings_from_unique_scores(scored, Some(k), control);
            }
            ef = ef
                .checked_mul(2)
                .unwrap_or(self.nodes.len())
                .min(self.nodes.len());
        }
    }

    pub(super) fn search_above_threshold(
        &self,
        query: &[f32],
        threshold: f32,
        control: Control<'_>,
    ) -> StorageBackendResult<PostingList> {
        check(control)?;
        validate_vector_values(self.dimensions, query)?;
        if !threshold.is_finite() {
            return Err(StorageBackendError::Other(format!(
                "vector similarity threshold must be finite, got {threshold}"
            )));
        }
        let norm = vector_norm(query);
        let mut scored = VectorQueryBuffer::new(control);
        let mut current: Option<(DocId, f32)> = None;
        for entry in self.active.iter() {
            check(control)?;
            let (_, node_id) = entry?;
            let node = self.node(*node_id)?.ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "HNSW active map references missing node {}",
                    *node_id
                ))
            })?;
            let score = cosine_similarity_with_norms(query, &node.raw_vector, norm, node.norm);
            if score >= threshold {
                match current.as_mut() {
                    Some((document, best)) if *document == node.doc_id => *best = best.max(score),
                    _ => {
                        if let Some(previous) = current.replace((node.doc_id, score)) {
                            scored.push(previous)?;
                        }
                    }
                }
            }
        }
        if let Some(last) = current {
            scored.push(last)?;
        }
        postings_from_unique_scores(scored, None, control)
    }
}
