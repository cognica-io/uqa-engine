//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime-independent retrieval configuration shared by logical and physical plans.

#[derive(Clone, Debug)]
pub enum GatingSpec {
    /// Lucene-compatible softplus gating.
    Softplus,
    /// Raw signal score scales the fused logit.
    Pass,
    /// Sigmoid gating with the named feature.
    Sigmoid { feature: String },
    /// `ReLU` gate.
    ReLU,
    /// Swish gate.
    Swish,
    /// GELU gate.
    Gelu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalPriorMode {
    Authority,
    Recency,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MultiStageCutoff {
    /// Top-K results -- final cardinality is `k`.
    TopK(usize),
    /// Fractional cutoff -- final cardinality is `n * ratio`.
    Ratio(f64),
}

#[derive(Clone, Debug, Default)]
pub struct TemporalFilterIR {
    pub timestamp: Option<f64>,
    pub time_range: Option<(f64, f64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Out,
    In,
    Both,
}
