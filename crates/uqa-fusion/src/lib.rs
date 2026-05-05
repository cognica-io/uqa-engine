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

pub use attention::{AttentionFusion, MultiHeadAttentionFusion};
pub use boolean::ProbabilisticBoolean;
pub use learned::LearnedFusion;
pub use log_odds::{AdaptiveLogOddsFusion, LogOddsFusion, SignalQuality};
pub use query_features::{extract_query_features, N_QUERY_FEATURES};
