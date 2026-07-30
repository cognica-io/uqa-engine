//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Multi-signal fusion: log-odds, attention, learned weights.

pub mod attention;
pub mod boolean;
pub mod learned;
pub mod log_odds;
pub mod query_features;

pub use attention::{AttentionFusion, AttentionFusionState, MultiHeadAttentionFusion};
pub use boolean::ProbabilisticBoolean;
pub use learned::{LearnedFusion, LearnedFusionState};
pub use log_odds::{
    AdaptiveLogOddsFusion, LogOddsFusion, LogOddsFusionError, LogitGating, SignalQuality,
};
pub use query_features::{extract_query_features, QueryFeatureExtractor, N_QUERY_FEATURES};
