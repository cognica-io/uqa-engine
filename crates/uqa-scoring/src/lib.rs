//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relevance scoring: BM25, Bayesian BM25 with three-term posterior, and
//! probabilistic Boolean / log-odds combinators (Paper 3, Paper 4).

pub mod bayesian;
pub mod bayesian_bm25;
pub mod bm25;
pub mod calibration;
pub mod prob;
pub mod scorer;
pub mod wand;

pub use bayesian::BayesianProbabilityTransform;
pub use bayesian_bm25::{BayesianBM25Params, BayesianBM25Scorer};
pub use bm25::{BM25Params, BM25Scorer};
pub use calibration::CalibrationMetrics;
pub use prob::{
    cosine_to_probability, log_odds_conjunction, logit, prob_and, prob_not, prob_or, sigmoid,
    PROB_EPSILON,
};
pub use scorer::Scorer;
pub use wand::{
    BlockMaxWandScorer, BoundTightnessAnalyzer, WandQuery, WandResult, WandScorer, WandStats,
};
