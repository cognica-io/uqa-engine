//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared top-k heap, result statistics, and bound validation.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use uqa_core::{DocId, PostingList};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub(super) const INF_DOC: u64 = u64::MAX;

/// Min-heap entry by score for top-k selection.
#[derive(Debug, Clone, Copy)]
pub(super) struct HeapEntry {
    pub(super) score: f64,
    pub(super) doc_id: DocId,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score && self.doc_id == other.doc_id
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // `BinaryHeap` is a max-heap; flip the score comparison so the
        // root holds the *minimum* score (the eviction candidate). On a
        // score tie, the entry with the *larger* doc id sits at the
        // root, matching the conventional "lower doc id wins" tie break
        // applied at output time.
        match other.score.total_cmp(&self.score) {
            Ordering::Equal => self.doc_id.cmp(&other.doc_id),
            ord => ord,
        }
    }
}

pub(super) fn update_top_k(
    top_k: &mut BinaryHeap<HeapEntry>,
    k: usize,
    score: f64,
    doc_id: DocId,
    threshold: &mut f64,
) {
    let candidate = HeapEntry { score, doc_id };
    if top_k.len() < k {
        top_k.push(candidate);
        if top_k.len() == k {
            *threshold = top_k.peek().map_or(0.0, |entry| entry.score);
        }
        return;
    }
    let Some(eviction) = top_k.peek() else {
        return;
    };
    if score > eviction.score || (score == eviction.score && doc_id < eviction.doc_id) {
        top_k.pop();
        top_k.push(candidate);
        *threshold = top_k.peek().map_or(*threshold, |entry| entry.score);
    }
}

/// Stats collected during a top-k pass; tests use these to assert the
/// exit-criterion skip rates from the master plan.
///
/// Skip rate semantics are `1 - scored / total_candidates`. The materialized
/// path reports the exact union of posting-list document ids. The score-cursor
/// path deliberately avoids a complete pre-scan and reports the sum of term
/// document frequencies, a safe upper bound on that union. `scored` counts
/// documents for which the complete query score was evaluated;
/// `cursor_advances` counts pivot-driven skips and is informational only.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct WANDStats {
    pub scored: u64,
    pub total_candidates: u64,
    pub cursor_advances: u64,
}

impl WANDStats {
    pub fn skip_rate(&self) -> f64 {
        if self.total_candidates == 0 {
            0.0
        } else {
            1.0 - (self.scored as f64 / self.total_candidates as f64)
        }
    }
}

#[derive(Debug, Clone)]
pub struct WANDResult {
    pub top_k: PostingList,
    pub stats: WANDStats,
}

pub(super) fn invalid_wand_input(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(format!("invalid WAND input: {}", message.into()))
}

pub(super) fn require_nonnegative_finite(value: f64, name: &str) -> StorageBackendResult<()> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(invalid_wand_input(format!(
            "{name} must be finite and non-negative, got {value}"
        )))
    }
}
