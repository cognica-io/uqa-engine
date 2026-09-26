//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A unique document stream needs only its current top-k heap, not a corpus-sized score map.

use super::DiskANNDocumentScore;
use crate::{
    read_control::StorageReadControl,
    vector_index::query::{postings_from_unique_scores, VectorQueryBuffer},
    StorageBackendResult,
};
use std::cmp::Ordering;
use uqa_core::{
    memory::{BudgetedBinaryHeap, BudgetedMap},
    DocId, PostingList,
};

struct Ranked {
    document: DocId,
    score: f32,
}

impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| self.document.cmp(&other.document))
    }
}
impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl Eq for Ranked {}

pub(in crate::diskann_index) struct TopK {
    heap: BudgetedBinaryHeap<Ranked>,
    limit: usize,
    identities: Option<BudgetedMap<DocId, ()>>,
}

impl TopK {
    pub(in crate::diskann_index) fn new(limit: usize, control: &StorageReadControl) -> Self {
        Self {
            heap: BudgetedBinaryHeap::new(control.memory()),
            limit,
            identities: None,
        }
    }

    pub(in crate::diskann_index) fn len(&self) -> usize {
        self.heap.len()
    }

    /// Repeated candidates on one fixed canonical view have the same full tensor score. Track only identities currently in the heap, bounded by k.
    pub(in crate::diskann_index) fn deduplicating(
        limit: usize,
        control: &StorageReadControl,
    ) -> Self {
        Self {
            identities: Some(BudgetedMap::new(control.memory())),
            ..Self::new(limit, control)
        }
    }

    pub(in crate::diskann_index) fn offer(
        &mut self,
        score: DiskANNDocumentScore,
    ) -> StorageBackendResult<()> {
        if self
            .identities
            .as_ref()
            .is_some_and(|ids| ids.get(&score.document).is_some())
        {
            return Ok(());
        }
        let value = Ranked {
            document: score.document,
            score: score.score,
        };
        if self.heap.len() < self.limit {
            if let Some(ids) = &mut self.identities {
                ids.insert(value.document, ())?;
            }
            self.heap.push(value)?;
        } else if self.heap.peek().is_some_and(|worst| value < *worst) {
            let removed = self.heap.pop().expect("nonempty selection");
            if let Some(ids) = &mut self.identities {
                ids.remove(&removed.document);
                ids.insert(value.document, ())?;
            }
            self.heap.push(value)?;
        }
        Ok(())
    }

    pub(in crate::diskann_index) fn finish(
        self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<PostingList> {
        control.check()?;
        drop(self.identities);
        let values = self.heap.into_vec();
        let mut scores = VectorQueryBuffer::new(Some(control));
        scores.reserve(values.len())?;
        for value in values.iter() {
            control.check()?;
            scores.push((value.document, value.score))?;
        }
        drop(values);
        scores.sort_unstable_by_key(|&(document, _)| document);
        postings_from_unique_scores(scores, None, Some(control))
    }
}
