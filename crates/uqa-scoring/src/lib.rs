//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relevance scoring: BM25, Lucene-style query-level Bayesian BM25, and
//! probabilistic Boolean and log-odds combinators.

pub mod bayesian;
pub mod bayesian_bm25;
pub mod bayesian_estimator;
pub mod bm25;
pub mod calibration;
pub mod calibration_validation;
pub mod error;
pub mod external_prior;
pub mod fusion_wand;
pub mod metrics;
pub mod multi_field;
pub mod parameter_learner;
pub mod prob;
pub mod score_domain;
pub mod scorer;
pub mod vector_calibration;
pub mod vector_score;
pub mod wand;

pub use bayesian::LegacyCompositePriorTransform;
pub use bayesian_bm25::{BayesianBM25Params, BayesianBM25Scorer};
pub use bayesian_estimator::UnsupervisedBm25ScoreEstimator;
pub use bm25::{BM25Params, BM25Scorer};
pub use calibration::{
    CalibrationMetrics, CalibrationReport, ReliabilityBin, VectorProbabilityTransform,
};
pub use calibration_validation::{
    BinaryDecisionMetrics, BootstrapConfig, ConfidenceInterval, HeldOutCalibrationGate,
    HeldOutCalibrationReport, ThresholdTransferReport,
};
pub use error::{ScoringError, ScoringResult};
pub use external_prior::{authority_prior, recency_prior, ExternalPriorScorer, PriorFn};
pub use fusion_wand::{FusionWANDScorer, TightenedFusionWANDScorer};
pub use metrics::{average_precision_at_k, dcg_at_k, mean_average_precision_at_k, ndcg_at_k};
pub use multi_field::{FieldConfig, MultiFieldBayesianScorer};
pub use parameter_learner::ParameterLearner;
pub use prob::{
    cosine_to_probability, log_odds_conjunction, logit, prob_and, prob_not, prob_or, sigmoid,
    PROB_EPSILON,
};
pub use score_domain::{EvidenceLogit, PosteriorProbability, PriorLogit, RawBm25Score};
pub use scorer::Scorer;
pub use vector_calibration::{
    VectorCalibrationModel, VectorCalibrationProvenance, VectorCalibrationStabilityReport,
    VectorCalibrationTarget, VECTOR_CALIBRATION_MODEL_SCHEMA_VERSION,
};
pub use vector_score::VectorScorer;
pub use wand::{
    AdaptiveWANDScorer, BlockMaxWANDScorer, BoundTightnessAnalyzer, CursorBlockMaxWANDScorer,
    CursorWANDQuery, CursorWANDScorer, WANDQuery, WANDResult, WANDScorer, WANDStats,
};
