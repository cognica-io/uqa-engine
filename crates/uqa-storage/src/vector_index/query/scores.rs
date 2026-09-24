//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Candidate reduction and posting construction retain their separate simultaneous buffers.

use std::collections::BTreeMap;

use uqa_core::{memory::BudgetedMap, DocId, Payload, PostingEntry, PostingList};

use super::{check, VectorQueryBuffer};
use crate::{
    read_control::StorageReadControl,
    vector_index::{cosine_similarity, deduplicate_scored_values, select_top_k_scored},
    StorageBackendResult,
};

/// Score validated provider candidates without replaying their iterator. Tensor reduction follows input order, including the existing floating-point maximum semantics; the resulting postings retain the ordinary caller-owned result boundary.
pub fn scored_posting_list<'a>(
    query: &[f32],
    entries: impl IntoIterator<Item = (DocId, &'a [f32])>,
    k: usize,
    control: Option<&StorageReadControl>,
) -> StorageBackendResult<PostingList> {
    check(control)?;
    let mut best = match control {
        Some(control) => BestScores::Controlled(BudgetedMap::new(control.memory())),
        None => BestScores::Ordinary(BTreeMap::new()),
    };
    for (doc_id, vector) in entries {
        check(control)?;
        best.update(doc_id, cosine_similarity(query, vector))?;
    }
    postings_from_unique_scores(best.into_scores(control)?, Some(k), control)
}

enum BestScores {
    Ordinary(BTreeMap<DocId, f32>),
    Controlled(BudgetedMap<DocId, f32>),
}

impl BestScores {
    fn update(&mut self, doc_id: DocId, score: f32) -> StorageBackendResult<()> {
        match self {
            Self::Ordinary(scores) => {
                scores
                    .entry(doc_id)
                    .and_modify(|best| *best = best.max(score))
                    .or_insert(score);
            }
            Self::Controlled(scores) => {
                if let Some(best) = scores.get_mut(&doc_id) {
                    *best = best.max(score);
                } else {
                    scores.insert(doc_id, score)?;
                }
            }
        }
        Ok(())
    }

    fn into_scores(
        self,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<VectorQueryBuffer<(DocId, f32)>> {
        let mut output = VectorQueryBuffer::new(control);
        match self {
            Self::Ordinary(scores) => {
                output.reserve(scores.len())?;
                for value in scores {
                    check(control)?;
                    output.push(value)?;
                }
            }
            Self::Controlled(scores) => {
                output.reserve(scores.len())?;
                for (&doc_id, &score) in &scores {
                    check(control)?;
                    output.push((doc_id, score))?;
                }
            }
        }
        Ok(output)
    }
}

pub(crate) fn postings_from_scores(
    mut scores: VectorQueryBuffer<(DocId, f32)>,
    limit: Option<usize>,
    control: Option<&StorageReadControl>,
) -> StorageBackendResult<PostingList> {
    check(control)?;
    let count = deduplicate_scored_values(&mut scores);
    scores.truncate(count);
    postings_from_unique_scores(scores, limit, control)
}

pub(crate) fn postings_from_unique_scores(
    scores: VectorQueryBuffer<(DocId, f32)>,
    limit: Option<usize>,
    control: Option<&StorageReadControl>,
) -> StorageBackendResult<PostingList> {
    check(control)?;
    let mut scores = scores.into_parts();
    if let Some(limit) = limit {
        select_top_k_scored(&mut scores.0, limit);
        scores.0.sort_unstable_by_key(|(doc_id, _)| *doc_id);
    }
    let mut postings = VectorQueryBuffer::new(control);
    postings.reserve(scores.0.len())?;
    for &(doc_id, score) in &scores.0 {
        check(control)?;
        postings.push(PostingEntry::new(
            doc_id,
            Payload::with_score(f64::from(score)),
        ))?;
    }
    // PostingList is the existing caller-owned result boundary; source and scratch leases remain independent of the transferred result.
    let (postings, _memory) = postings.into_parts();
    Ok(PostingList::from_sorted_unchecked(postings))
}
