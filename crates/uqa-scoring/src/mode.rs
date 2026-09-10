//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text retrieval scoring configuration.

use crate::{BM25Params, BayesianBM25Params};

/// Scoring strategy for a text retrieval leaf.
#[derive(Debug, Clone)]
pub enum ScoringMode {
    BM25(BM25Params),
    BayesianBM25(BayesianBM25Params),
}

impl Default for ScoringMode {
    fn default() -> Self {
        Self::BM25(BM25Params::default())
    }
}
