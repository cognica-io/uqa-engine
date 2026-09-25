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
use uqa_core::{memory::BudgetedBinaryHeap, DocId, PostingList};

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

pub(super) struct TopK {
    heap: BudgetedBinaryHeap<Ranked>,
    limit: usize,
}

impl TopK {
    pub(super) fn new(limit: usize, control: &StorageReadControl) -> Self {
        Self {
            heap: BudgetedBinaryHeap::new(control.memory()),
            limit,
        }
    }

    pub(super) fn offer(&mut self, score: DiskANNDocumentScore) -> StorageBackendResult<()> {
        let value = Ranked {
            document: score.document,
            score: score.score,
        };
        if self.heap.len() < self.limit {
            self.heap.push(value)?;
        } else if self.heap.peek().is_some_and(|worst| value < *worst) {
            let _removed = self.heap.pop();
            self.heap.push(value)?;
        }
        Ok(())
    }

    pub(super) fn finish(self, control: &StorageReadControl) -> StorageBackendResult<PostingList> {
        control.check()?;
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
