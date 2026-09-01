//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! WAND and Block-Max WAND top-k scorers (Section 6, Paper 3).
//!
//! Both implementations advance posting-list cursors through pivot
//! resolution. Pruning is *exact* under their respective upper-bound
//! contracts: for WAND the per-term `term_upper_bound(df)`; for BMW the
//! tighter per-block max stored in [`BlockMaxIndex`]. The output top-k

mod common;
mod cursor;
mod diagnostics;
mod materialized;

pub use common::{WANDResult, WANDStats};
pub use cursor::{CursorBlockMaxWANDScorer, CursorWANDQuery, CursorWANDScorer};
pub use diagnostics::{AdaptiveWANDScorer, BoundTightnessAnalyzer};
pub use materialized::{BlockMaxWANDScorer, WANDQuery, WANDScorer};

#[cfg(test)]
use materialized::candidate_union;

#[cfg(test)]
mod tests;
