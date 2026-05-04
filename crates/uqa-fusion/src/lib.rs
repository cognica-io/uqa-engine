//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Multi-signal fusion: log-odds, attention, learned weights.

pub mod boolean;
pub mod log_odds;

pub use boolean::ProbabilisticBoolean;
pub use log_odds::{AdaptiveLogOddsFusion, LogOddsFusion, SignalQuality};
