//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact readers retain canonical vectors and their allowance independently of the live session.

use std::sync::Arc;

use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId, Payload, PostingEntry, PostingList,
};

use crate::read_control::StorageReadControl;
use crate::vector_index::{
    cosine_similarity, select_top_k_scored, validate_vector_values, VectorIndex,
};
use crate::{StorageBackendError, StorageBackendResult};

mod builder;
pub use builder::RetainedVectorIndexBuilder;

pub(crate) type VectorEntries = Vec<(DocId, u32, Vec<f32>)>;

/// An immutable exact index whose vector buffers and read workspace share the original retention allowance.
#[derive(Clone)]
pub struct RetainedVectorIndex {
    entries: Arc<Budgeted<VectorEntries>>,
    dimensions: u32,
    index_kind: &'static str,
    control: StorageReadControl,
}

impl RetainedVectorIndex {
    pub(crate) fn from_entries(
        entries: Budgeted<VectorEntries>,
        dimensions: u32,
        index_kind: &'static str,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        Ok(Self {
            entries: entries.into_shared()?,
            dimensions,
            index_kind,
            control: control.clone(),
        })
    }

    fn scores(
        &self,
        query: &[f32],
        threshold: Option<f32>,
    ) -> StorageBackendResult<BudgetedVec<(DocId, f32)>> {
        let mut scores = BudgetedVec::new(self.control.memory());
        for vectors in self.entries.chunk_by(|a, b| a.0 == b.0) {
            self.control.check()?;
            let mut best = cosine_similarity(query, &vectors[0].2);
            for (_, _, vector) in &vectors[1..] {
                self.control.check()?;
                let score = cosine_similarity(query, vector);
                if score > best {
                    best = score;
                }
            }
            if threshold.is_none_or(|threshold| best >= threshold) {
                scores.push((vectors[0].0, best))?;
            }
        }
        Ok(scores)
    }

    fn postings(&self, scores: &[(DocId, f32)]) -> StorageBackendResult<PostingList> {
        let mut postings = BudgetedVec::new(self.control.memory());
        postings.reserve(scores.len())?;
        for &(doc_id, score) in scores {
            self.control.check()?;
            postings.push(PostingEntry::new(
                doc_id,
                Payload::with_score(f64::from(score)),
            ))?;
        }
        let (postings, _memory) = postings.into_parts();
        Ok(PostingList::from_sorted_unchecked(postings))
    }
}

impl VectorIndex for RetainedVectorIndex {
    fn contains_document(&self, doc_id: DocId) -> StorageBackendResult<bool> {
        self.control.check()?;
        Ok(self
            .entries
            .binary_search_by_key(&doc_id, |entry| entry.0)
            .is_ok())
    }

    fn dimensions(&self) -> u32 {
        self.dimensions
    }
    fn index_kind(&self) -> &'static str {
        self.index_kind
    }
    fn add(&mut self, _: DocId, _: Vec<f32>) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn add_many(&mut self, _: DocId, _: Vec<Vec<f32>>) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn delete(&mut self, _: DocId) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn clear(&mut self) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn initialize(&mut self) -> StorageBackendResult<()> {
        Err(read_only())
    }
    fn search_knn(&self, query: &[f32], k: usize) -> StorageBackendResult<PostingList> {
        self.control.check()?;
        validate_vector_values(self.dimensions, query)?;
        if k == 0 {
            return Ok(PostingList::new());
        }
        // The tuple drops score storage before its reservation on every return path.
        let mut scores = self.scores(query, None)?.into_parts();
        select_top_k_scored(&mut scores.0, k);
        scores.0.sort_unstable_by_key(|(doc_id, _)| *doc_id);
        self.postings(&scores.0)
    }
    fn search_threshold(&self, query: &[f32], threshold: f32) -> StorageBackendResult<PostingList> {
        self.control.check()?;
        validate_vector_values(self.dimensions, query)?;
        if !threshold.is_finite() {
            return Err(StorageBackendError::Other(format!(
                "vector similarity threshold must be finite, got {threshold}"
            )));
        }
        let scores = self.scores(query, Some(threshold))?;
        self.postings(&scores)
    }
    fn count(&self) -> StorageBackendResult<usize> {
        self.control.check()?;
        Ok(self.entries.len())
    }
    fn snapshot(&self) -> StorageBackendResult<Arc<dyn VectorIndex>> {
        Ok(Arc::new(self.clone()))
    }
}

fn read_only() -> StorageBackendError {
    StorageBackendError::Other("cannot write a retained vector snapshot".into())
}

#[cfg(test)]
mod tests;
