//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Tensor maxima spill by document; final top-K selection retains only K scores.

use super::{postings_from_unique_scores, VectorQueryBuffer};
use crate::{
    read_control::StorageReadControl,
    spill_map::{invalid, Map, Record},
    StorageBackendResult,
};
use std::cmp::Ordering;
use uqa_core::{memory::BudgetedBinaryHeap, DocId, PostingList};

pub struct SpillingVectorScores {
    scores: Map<f32>,
    control: StorageReadControl,
}

impl SpillingVectorScores {
    pub fn new(control: &StorageReadControl) -> Self {
        Self {
            scores: Map::new(control.memory(), control.memory().limit() / 4),
            control: control.clone(),
        }
    }
    pub fn add(&mut self, document: DocId, score: f32) -> StorageBackendResult<()> {
        self.control.check()?;
        let best = self
            .scores
            .get(u128::from(document))?
            .map_or(score, |value| (*value).max(score));
        self.scores
            .insert(u128::from(document), best, Some(&self.control))
    }
    pub fn finish(self, k: usize) -> StorageBackendResult<PostingList> {
        self.control.check()?;
        let mut heap = BudgetedBinaryHeap::new(self.control.memory());
        for entry in self.scores.iter() {
            self.control.check()?;
            let (document, score) = entry?;
            let entry = Worst {
                document: DocId::try_from(document)
                    .map_err(|_| uqa_core::memory::MemoryError::SizeOverflow)?,
                score: *score,
            };
            if heap.len() < k {
                heap.push(entry)?;
            } else if heap.peek().is_some_and(|worst| entry < *worst) {
                heap.pop();
                heap.push(entry)?;
            }
        }
        drop(self.scores);
        let mut selected = VectorQueryBuffer::new(Some(&self.control));
        while let Some(entry) = heap.pop() {
            selected.push((entry.document, entry.score))?;
        }
        drop(heap);
        postings_from_unique_scores(selected, Some(k), Some(&self.control))
    }
}

#[derive(Clone, Copy)]
struct Worst {
    document: DocId,
    score: f32,
}
impl PartialEq for Worst {
    fn eq(&self, other: &Self) -> bool {
        self.document == other.document && self.score.to_bits() == other.score.to_bits()
    }
}
impl Eq for Worst {}
impl PartialOrd for Worst {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Worst {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| self.document.cmp(&other.document))
    }
}

impl Record for f32 {
    fn memory_bytes(&self) -> StorageBackendResult<usize> {
        Ok(size_of::<Self>())
    }
    fn encoded_bytes(&self) -> StorageBackendResult<usize> {
        Ok(4)
    }
    fn encode(&self, bytes: &mut Vec<u8>) {
        bytes.extend_from_slice(&self.to_bits().to_le_bytes());
    }
    fn decode(bytes: &[u8]) -> StorageBackendResult<Self> {
        Ok(Self::from_bits(u32::from_le_bytes(
            bytes.try_into().map_err(|_| invalid("score width"))?,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_index::query::scores::postings_from_scores;

    #[test]
    fn tensor_maxima_spill_and_preserve_score_bits_top_k_and_document_ties() {
        let mut input = Vec::new();
        for document in (0..1200).rev() {
            input.push((document, (document % 17) as f32 / 17.0));
            input.push((document, 0.25));
        }
        input.extend([(1201, -0.0), (1201, 0.0), (1202, f32::NAN), (1202, 1.0)]);
        for k in [0, 1, 10, 120] {
            let control = StorageReadControl::with_limit(128 * 1024);
            let mut spilled = SpillingVectorScores::new(&control);
            for &(document, score) in &input {
                spilled.add(document, score).unwrap();
            }
            assert!(spilled.scores.is_spilled());
            let mut reference = VectorQueryBuffer::new(None);
            for &entry in &input {
                reference.push(entry).unwrap();
            }
            let expected = postings_from_scores(reference, Some(k), None).unwrap();
            let result = spilled.finish(k).unwrap();
            assert_eq!(result, expected);
            assert!(control.memory().peak() <= control.memory().limit());
            assert_eq!(control.memory().used(), 0);
        }
    }

    #[test]
    fn spilled_special_scores_preserve_maximum_order_and_result_bits() {
        let input = [
            (1, -0.0),
            (1, 0.0),
            (2, f32::NAN),
            (3, f32::NEG_INFINITY),
            (4, f32::INFINITY),
            (5, f32::from_bits(0x7fc0_0001)),
            (5, 1.0),
            (6, 1.0),
            (7, 0.0),
            (7, -0.0),
        ];
        for k in [0, 1, 10] {
            let control = StorageReadControl::with_limit(32 * 1024);
            let mut spilled = SpillingVectorScores::new(&control);
            let mut reference = std::collections::BTreeMap::<DocId, f32>::new();
            for (document, score) in input {
                spilled.add(document, score).unwrap();
                reference
                    .entry(document)
                    .and_modify(|value| *value = value.max(score))
                    .or_insert(score);
            }
            spilled.scores.spill(Some(&control)).unwrap();
            assert!(spilled.scores.is_spilled());
            let mut scores = VectorQueryBuffer::new(None);
            for entry in reference {
                scores.push(entry).unwrap();
            }
            let expected = postings_from_unique_scores(scores, Some(k), None).unwrap();
            let actual = spilled.finish(k).unwrap();
            let bits = |entries: &PostingList| {
                entries
                    .iter()
                    .map(|entry| (entry.doc_id, entry.payload.score.to_bits()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(bits(&actual), bits(&expected));
            assert_eq!(control.memory().used(), 0);
        }
    }

    #[test]
    fn spilled_scores_cancel_without_leaking_or_admitting_an_oversized_result() {
        let control = StorageReadControl::with_limit(32 * 1024);
        let mut scores = SpillingVectorScores::new(&control);
        for document in 0..1000 {
            scores.add(document, 1.0).unwrap();
        }
        assert!(scores.scores.is_spilled());
        assert!(scores.finish(1000).is_err());
        assert_eq!(control.memory().used(), 0);
        let mut scores = SpillingVectorScores::new(&control);
        scores.add(1, 1.0).unwrap();
        control.cancellation().cancel();
        assert!(scores.finish(1).is_err());
        assert_eq!(control.memory().used(), 0);
    }
}
