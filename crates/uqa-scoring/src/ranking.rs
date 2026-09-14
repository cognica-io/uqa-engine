//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deterministic score ordering shared by retrieval consumers.

use std::cmp::Ordering;
use uqa_core::{PostingList, ScoredEntry};

pub fn rank_top_k(pl: &PostingList, top_k: usize) -> Vec<ScoredEntry> {
    let entries: Vec<ScoredEntry> = pl.iter().map(ScoredEntry::from_entry).collect();
    rank_scored_entries_top_k(entries, top_k)
}

pub fn rank_scored_entries_top_k(mut entries: Vec<ScoredEntry>, top_k: usize) -> Vec<ScoredEntry> {
    if top_k == 0 {
        return Vec::new();
    }
    if top_k < entries.len() {
        entries.select_nth_unstable_by(top_k, compare_scored_entry_desc);
        entries.truncate(top_k);
    }
    entries.sort_by(compare_scored_entry_desc);
    entries.truncate(top_k);
    entries
}

fn compare_scored_entry_desc(a: &ScoredEntry, b: &ScoredEntry) -> Ordering {
    b.score
        .total_cmp(&a.score)
        .then_with(|| a.doc_id.cmp(&b.doc_id))
}
