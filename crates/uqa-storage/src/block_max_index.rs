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

use rusqlite::params;

use uqa_core::PostingList;

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
        Self::new(DEFAULT_BLOCK_SIZE)
    }
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

    pub fn set_block_maxes(&mut self, table: &str, field: &str, term: &str, scores: Vec<f64>) {
        self.block_maxes.insert(
            (table.to_string(), field.to_string(), term.to_string()),
            scores,
        );
    }

    /// Compute and store per-block maxima for `posting_list`. Each
    /// block's max is `max_{e in block} scorer.score(tf(e), tf(e), df)`,
    /// matching the canonical UQA behavior (which uses tf as a stand-in for
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

    pub fn save_to_sqlite(&self, conn: &rusqlite::Connection) -> rusqlite::Result<()> {
        ensure_global_blockmax_shape(conn)?;
        conn.execute("DELETE FROM _global_blockmax", [])?;
        for ((table, field, term), scores) in &self.block_maxes {
            for (block_idx, score) in scores.iter().enumerate() {
                conn.execute(
                    "INSERT INTO _global_blockmax
                        (table_name, field, term, block_idx, max_score)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![table, field, term, block_idx as i64, *score],
                )?;
            }
        }
        Ok(())
    }

    pub fn load_from_sqlite(&mut self, conn: &rusqlite::Connection) -> rusqlite::Result<()> {
        ensure_global_blockmax_shape(conn)?;
        self.clear();
        let mut stmt = conn.prepare(
            "SELECT table_name, field, term, block_idx, max_score
             FROM _global_blockmax
             ORDER BY table_name, field, term, block_idx",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, f64>(4)?,
            ))
        })?;
        for row in rows {
            let (table, field, term, block_idx, score) = row?;
            let entry = self.block_maxes.entry((table, field, term)).or_default();
            let idx = block_idx.max(0) as usize;
            if entry.len() <= idx {
                entry.resize(idx + 1, 0.0);
            }
            entry[idx] = score;
        }
        Ok(())
    }
}

fn ensure_global_blockmax_shape(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS _global_blockmax (
            table_name TEXT NOT NULL DEFAULT '',
            field     TEXT NOT NULL,
            term      TEXT NOT NULL,
            block_idx INTEGER NOT NULL,
            max_score REAL NOT NULL,
            PRIMARY KEY (table_name, field, term, block_idx)
        )",
        [],
    )?;
    let mut stmt = conn.prepare("PRAGMA table_info(_global_blockmax)")?;
    let cols = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    if !cols.iter().any(|c| c == "table_name") {
        conn.execute(
            "ALTER TABLE _global_blockmax ADD COLUMN table_name TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    Ok(())
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
