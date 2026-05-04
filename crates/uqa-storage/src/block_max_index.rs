//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Per-block maximum score index for Block-Max WAND optimisation.
//!
//! For each `(table, field, term)` posting list, we precompute the
//! maximum score reachable inside each fixed-size block of consecutive
//! entries. Block-Max WAND consults these tighter per-block upper bounds
//! during pivot resolution, achieving higher skip rates than plain WAND
//! (Theorem 6.2.2, Paper 3).

use std::collections::BTreeMap;

use uqa_core::PostingList;

pub const DEFAULT_BLOCK_SIZE: usize = 128;

/// Trait every scorer used to seed a block-max index implements. Only
/// the inner BM25 surface is needed; `uqa_scoring::BM25Scorer` already
/// satisfies it without extra glue.
pub trait BlockMaxScorer {
    fn score(&self, term_freq: u64, doc_length: u64, doc_freq: u64) -> f64;
}

#[derive(Debug, Default, Clone)]
pub struct BlockMaxIndex {
    block_size: usize,
    block_maxes: BTreeMap<(String, String, String), Vec<f64>>,
}

impl BlockMaxIndex {
    pub fn new(block_size: usize) -> Self {
        debug_assert!(block_size > 0, "block_size must be > 0");
        Self {
            block_size,
            block_maxes: BTreeMap::new(),
        }
    }

    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// Compute and store per-block maxima for `posting_list`. Each
    /// block's max is `max_{e in block} scorer.score(tf(e), tf(e), df)`,
    /// matching the Python reference (which uses tf as a stand-in for
    /// doc length when none is supplied).
    pub fn build<S: BlockMaxScorer + ?Sized>(
        &mut self,
        posting_list: &PostingList,
        scorer: &S,
        field: &str,
        term: &str,
        table: &str,
    ) {
        let entries = posting_list.entries();
        let key = (table.to_string(), field.to_string(), term.to_string());
        if entries.is_empty() {
            self.block_maxes.insert(key, Vec::new());
            return;
        }
        let df = entries.len() as u64;
        let mut blocks = Vec::with_capacity(entries.len().div_ceil(self.block_size));
        for chunk in entries.chunks(self.block_size) {
            let mut max_score = 0.0_f64;
            for entry in chunk {
                let positions = &entry.payload.positions;
                let tf = if positions.is_empty() {
                    1
                } else {
                    positions.len() as u64
                };
                let s = scorer.score(tf, tf, df);
                if s > max_score {
                    max_score = s;
                }
            }
            blocks.push(max_score);
        }
        self.block_maxes.insert(key, blocks);
    }

    pub fn block_max(&self, table: &str, field: &str, term: &str, block_idx: usize) -> f64 {
        let key = (table.to_string(), field.to_string(), term.to_string());
        self.block_maxes
            .get(&key)
            .and_then(|v| v.get(block_idx).copied())
            .unwrap_or(0.0)
    }

    pub fn num_blocks(&self, table: &str, field: &str, term: &str) -> usize {
        let key = (table.to_string(), field.to_string(), term.to_string());
        self.block_maxes.get(&key).map_or(0, Vec::len)
    }

    /// Block index for a given posting-list cursor position.
    pub fn block_index_for(&self, position: usize) -> usize {
        position / self.block_size
    }

    pub fn clear(&mut self) {
        self.block_maxes.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{Payload, PostingEntry, PostingList};

    /// Trivial scorer: tf raised to a constant — strictly increasing in
    /// tf so the per-block max equals the max tf in the block.
    struct LinearScorer;
    impl BlockMaxScorer for LinearScorer {
        fn score(&self, term_freq: u64, _doc_length: u64, _doc_freq: u64) -> f64 {
            term_freq as f64
        }
    }

    fn pl_with_tfs(tfs: &[u32]) -> PostingList {
        let entries: Vec<PostingEntry> = tfs
            .iter()
            .enumerate()
            .map(|(i, &tf)| {
                let positions = (0..tf).collect();
                PostingEntry::new(
                    (i as u64) + 1,
                    Payload {
                        positions,
                        score: 0.0,
                        fields: BTreeMap::default(),
                    },
                )
            })
            .collect();
        PostingList::from_unsorted(entries)
    }

    #[test]
    fn block_max_records_per_block_maximum() {
        let mut idx = BlockMaxIndex::new(2);
        let pl = pl_with_tfs(&[1, 5, 3, 7, 2]);
        idx.build(&pl, &LinearScorer, "title", "rust", "articles");
        // Blocks of size 2: [1, 5] [3, 7] [2] -> maxes 5, 7, 2
        assert_eq!(idx.num_blocks("articles", "title", "rust"), 3);
        assert!((idx.block_max("articles", "title", "rust", 0) - 5.0).abs() < 1e-12);
        assert!((idx.block_max("articles", "title", "rust", 1) - 7.0).abs() < 1e-12);
        assert!((idx.block_max("articles", "title", "rust", 2) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn empty_posting_list_records_no_blocks() {
        let mut idx = BlockMaxIndex::new(4);
        idx.build(&PostingList::new(), &LinearScorer, "title", "rust", "t");
        assert_eq!(idx.num_blocks("t", "title", "rust"), 0);
        assert!((idx.block_max("t", "title", "rust", 0) - 0.0).abs() < 1e-12);
    }

    #[test]
    fn block_index_for_position() {
        let idx = BlockMaxIndex::new(4);
        assert_eq!(idx.block_index_for(0), 0);
        assert_eq!(idx.block_index_for(3), 0);
        assert_eq!(idx.block_index_for(4), 1);
        assert_eq!(idx.block_index_for(9), 2);
    }
}
