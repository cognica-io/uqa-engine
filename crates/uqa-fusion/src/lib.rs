//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Multi-signal combination with explicit contracts: exact signed Bayesian
//! evidence, robust positive-evidence pooling, attention, and learned weights.

pub mod attention;
pub mod bayesian_evidence;
pub mod boolean;
pub mod learned;
pub mod positive_evidence;
pub mod query_features;

pub use attention::{AttentionFusion, AttentionFusionState, MultiHeadAttentionFusion};
pub use bayesian_evidence::{BayesianEvidenceFusion, BayesianEvidenceFusionError};
pub use boolean::ProbabilisticBoolean;
pub use learned::{LearnedFusion, LearnedFusionState};
pub use positive_evidence::{
    AdaptivePositiveEvidencePool, LogitGating, PositiveEvidencePoolError,
    RobustPositiveEvidencePool, SignalQuality,
};
pub use query_features::{extract_query_features, QueryFeatureExtractor, N_QUERY_FEATURES};
