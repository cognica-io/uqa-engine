//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cardinality estimation for SQL predicates and cross-paradigm operator trees.
//!
//! The relational and operator-tree surfaces share configuration and statistics,
//! but remain separate estimator modules. Results guide planner costs; they are
//! heuristic estimates rather than query-correctness values or universal bounds.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{IndexStats, Predicate, Value};
use uqa_operators::{
    DeepFusionLayer, GraphPatternIR, MultiStageCutoff, OperatorTree, ProbBoolMode,
    TemporalFilterIR, VertexConstraint,
};
use uqa_sql::ast::{BinaryOp, Expr};

mod ast_helpers;
mod config;
mod cross_paradigm;
mod entropy;
mod filter;
mod graph;
mod join;
mod operator;
mod relational;
mod rpq_complexity;
mod sampling_rng;
mod stats;

pub use entropy::{column_entropy, entropy_cardinality_lower_bound, mutual_information_estimate};
pub use stats::{
    ColumnStats, EdgeSample, GraphStats, GraphStoreSampler, RelationStats, Selectivity,
    GRAPH_AVG_DEGREE_DEFAULT, JACCARD_JOIN_SELECTIVITY, VECTOR_JOIN_SELECTIVITY,
};

use ast_helpers::{
    column_of, compare_values, histogram_range_selectivity, literal_of, value_as_f64,
};
use rpq_complexity::rpq_label_count;
use sampling_rng::XorShiftRng;

#[derive(Clone, Default)]
pub struct CardinalityEstimator {
    /// Default selectivity for an unknown predicate.
    pub default_selectivity: f64,
    /// Default selectivity for a `LIKE 'foo%'` style prefix match.
    pub like_selectivity: f64,
    /// Default selectivity for an inequality predicate with no histogram.
    pub range_selectivity: f64,
    /// Per-column statistics keyed by column name.
    pub column_stats: BTreeMap<String, ColumnStats>,
    /// Optional graph statistics used by traversal and pattern heuristics.
    pub graph_stats: Option<GraphStats>,
    /// Optional graph store sampler for random-walk pattern estimates.
    pub graph_store: Option<Arc<dyn GraphStoreSampler>>,
}

#[cfg(test)]
mod tests;
