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

use crate::{StorageBackendError, StorageBackendResult};

pub const DEFAULT_BLOCK_SIZE: usize = 128;

/// Trait every scorer used to seed a block-max index implements. Only
/// the inner BM25 surface is needed; `uqa_scoring::BM25Scorer` already
/// satisfies it without extra glue.
pub trait BlockMaxScorer {
    fn score(&self, term_freq: u64, doc_length: u64, doc_freq: u64) -> f64;
}

#[derive(Debug, Clone)]
pub struct BlockMaxIndex {
    block_size: usize,
    block_maxes: BTreeMap<(String, String, String), Vec<f64>>,
}

impl Default for BlockMaxIndex {
    fn default() -> Self {
        Self {
            block_size: DEFAULT_BLOCK_SIZE,
            block_maxes: BTreeMap::new(),
        }
    }
}

impl BlockMaxIndex {
    pub fn new(block_size: usize) -> StorageBackendResult<Self> {
        if block_size == 0 {
            return Err(StorageBackendError::Other(
                "block-max block size must be greater than zero".to_string(),
            ));
        }
        Ok(Self {
            block_size,
            block_maxes: BTreeMap::new(),
        })
    }

    pub fn block_size(&self) -> usize {
        self.block_size
    }

    pub fn set_block_maxes(
        &mut self,
        table: &str,
        field: &str,
        term: &str,
        scores: Vec<f64>,
    ) -> StorageBackendResult<()> {
        validate_scores(&scores)?;
        self.block_maxes.insert(
            (table.to_string(), field.to_string(), term.to_string()),
            scores,
        );
        Ok(())
    }

    /// Compute and store per-block maxima for `posting_list`. Each
    /// block's max is `max_{e in block} scorer.score(tf(e), tf(e), df)` and
    /// uses term frequency as the document-length stand-in when none is given.
    pub fn build<S: BlockMaxScorer + ?Sized>(
        &mut self,
        posting_list: &PostingList,
        scorer: &S,
        field: &str,
        term: &str,
        table: &str,
    ) -> StorageBackendResult<()> {
        if self.block_size == 0 {
            return Err(StorageBackendError::Other(
                "block-max block size must be greater than zero".to_string(),
            ));
        }
        let entries = posting_list.entries();
        let key = (table.to_string(), field.to_string(), term.to_string());
        if entries.is_empty() {
            self.block_maxes.insert(key, Vec::new());
            return Ok(());
        }
        let df = u64::try_from(entries.len()).map_err(|_| {
            StorageBackendError::Other("posting-list length exceeds u64".to_string())
        })?;
        let mut blocks = Vec::with_capacity(entries.len().div_ceil(self.block_size));
        for chunk in entries.chunks(self.block_size) {
            let mut max_score = 0.0_f64;
            for entry in chunk {
                let positions = &entry.payload.positions;
                let tf = if positions.is_empty() {
                    1
                } else {
                    u64::try_from(positions.len()).map_err(|_| {
                        StorageBackendError::Other("term position count exceeds u64".to_string())
                    })?
                };
                let s = scorer.score(tf, tf, df);
                validate_score(s)?;
                if s > max_score {
                    max_score = s;
                }
            }
            blocks.push(max_score);
        }
        self.block_maxes.insert(key, blocks);
        Ok(())
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

    /// Borrow all block scores for one posting without repeated key
    /// construction. BMW uses this to precompute suffix bounds once per query.
    pub fn block_maxes(&self, table: &str, field: &str, term: &str) -> Option<&[f64]> {
        let key = (table.to_string(), field.to_string(), term.to_string());
        self.block_maxes.get(&key).map(Vec::as_slice)
    }

    /// Block index for a given posting-list cursor position.
    pub fn block_index_for(&self, position: usize) -> StorageBackendResult<usize> {
        if self.block_size == 0 {
            return Err(StorageBackendError::Other(
                "block-max block size must be greater than zero".to_string(),
            ));
        }
        Ok(position / self.block_size)
    }

    pub fn clear(&mut self) {
        self.block_maxes.clear();
    }

    /// Iterate validated block maxima for persistence without mutable storage access.
    pub fn entries(&self) -> impl Iterator<Item = ((&str, &str, &str), &[f64])> {
        self.block_maxes
            .iter()
            .map(|((table, field, term), scores)| {
                (
                    (table.as_str(), field.as_str(), term.as_str()),
                    scores.as_slice(),
                )
            })
    }
}

fn validate_scores(scores: &[f64]) -> StorageBackendResult<()> {
    for &score in scores {
        validate_score(score)?;
    }
    Ok(())
}

fn validate_score(score: f64) -> StorageBackendResult<()> {
    if score.is_finite() && score >= 0.0 {
        Ok(())
    } else {
        Err(StorageBackendError::Other(format!(
            "block-max score must be finite and non-negative, got {score}"
        )))
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

    struct InvalidScorer(f64);
    impl BlockMaxScorer for InvalidScorer {
        fn score(&self, _term_freq: u64, _doc_length: u64, _doc_freq: u64) -> f64 {
            self.0
        }
    }

    fn pl_with_tfs(tfs: &[u32]) -> PostingList {
        let entries: Vec<PostingEntry> = tfs
            .iter()
            .enumerate()
            .map(|(i, &tf)| {
                let positions = (0..tf).collect();
                PostingEntry::new(
                    u64::try_from(i).unwrap() + 1,
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
        let mut idx = BlockMaxIndex::new(2).unwrap();
        let pl = pl_with_tfs(&[1, 5, 3, 7, 2]);
        idx.build(&pl, &LinearScorer, "title", "rust", "articles")
            .unwrap();
        // Blocks of size 2: [1, 5] [3, 7] [2] -> maxes 5, 7, 2
        assert_eq!(idx.num_blocks("articles", "title", "rust"), 3);
        assert!((idx.block_max("articles", "title", "rust", 0) - 5.0).abs() < 1e-12);
        assert!((idx.block_max("articles", "title", "rust", 1) - 7.0).abs() < 1e-12);
        assert!((idx.block_max("articles", "title", "rust", 2) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn empty_posting_list_records_no_blocks() {
        let mut idx = BlockMaxIndex::new(4).unwrap();
        idx.build(&PostingList::new(), &LinearScorer, "title", "rust", "t")
            .unwrap();
        assert_eq!(idx.num_blocks("t", "title", "rust"), 0);
        assert!((idx.block_max("t", "title", "rust", 0) - 0.0).abs() < 1e-12);
    }

    #[test]
    fn block_index_for_position() {
        let idx = BlockMaxIndex::new(4).unwrap();
        assert_eq!(idx.block_index_for(0).unwrap(), 0);
        assert_eq!(idx.block_index_for(3).unwrap(), 0);
        assert_eq!(idx.block_index_for(4).unwrap(), 1);
        assert_eq!(idx.block_index_for(9).unwrap(), 2);
    }

    #[test]
    fn rejects_zero_block_size_and_invalid_scores_without_replacing_state() {
        assert!(BlockMaxIndex::new(0).is_err());

        let mut index = BlockMaxIndex::new(2).unwrap();
        index
            .set_block_maxes("docs", "body", "term", vec![3.0])
            .unwrap();
        let postings = pl_with_tfs(&[1, 2]);
        assert!(index
            .build(&postings, &InvalidScorer(f64::NAN), "body", "term", "docs")
            .is_err());
        assert_eq!(index.block_max("docs", "body", "term", 0), 3.0);
        assert!(index
            .set_block_maxes("docs", "body", "term", vec![-1.0])
            .is_err());
        assert_eq!(index.block_max("docs", "body", "term", 0), 3.0);
    }
}
