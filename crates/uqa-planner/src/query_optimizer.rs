//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `QueryOptimizer`: Rust implementation of UQA `planner/optimizer`.
//!
//! Walks an [`OperatorTree`] and applies the ten rewrite stages from
//! Theorem 6.1.2 (Paper 1) and Theorem 6.1.1 (Paper 2):
//!
//! 1. `simplify_algebra` -- address-independent idempotence /
//!    absorption / empty elimination on membership-only Intersect /
//!    Union operands. Score-bearing operands remain distinct because
//!    posting-list merges add their scores.
//! 2. `push_filters_down` -- sink Filter into Intersect children when
//!    the field applies.
//! 3. `push_graph_pattern_filters` -- fold vertex / edge property
//!    filters into PatternMatch constraints.
//! 4. `push_filter_into_traverse` -- absorb vertex predicates into
//!    Traverse so BFS prunes during expansion.
//! 5. `push_filter_below_graph_join` -- move filters past graph joins
//!    when the field belongs to the left side.
//! 6. `fuse_join_pattern` -- merge intersected PatternMatch operators
//!    that share a vertex variable.
//! 7. `merge_vector_thresholds` -- collapse adjacent
//!    VectorSimilarity(q, t1) AND VectorSimilarity(q, t2) into a
//!    single VectorSimilarity(q, max(t1, t2)).
//! 8. `reorder_intersect` -- sort Intersect children by estimated
//!    cardinality (cheapest first).
//! 9. `reorder_fusion_signals` -- sort fusion signals by cost; graph
//!    operators receive a 0.5x discount when graph stats are
//!    available.
//! 10. `apply_index_scan` -- substitute leaf Filter with IndexScan
//!     when a covering index is registered and cheaper.

use std::sync::Arc;

mod algebra;
mod graph_rewrites;
mod index_selection;
mod reorder;
mod tree_map;

use uqa_core::Predicate;
use uqa_operators::OperatorTree;
use uqa_storage::IndexManager;

use crate::cardinality::{CardinalityEstimator, GraphStats};
use crate::cost_model::{CostEstimator, CostModel};

/// Fluent configuration for the optimizer pipeline. Lets callers
/// disable individual stages for testing without poking at private
/// fields.
#[derive(Debug, Clone)]
pub struct OptimizerConfig {
    pub enable_simplify_algebra: bool,
    pub enable_push_filters_down: bool,
    pub enable_push_graph_pattern_filters: bool,
    pub enable_push_filter_into_traverse: bool,
    pub enable_push_filter_below_graph_join: bool,
    pub enable_fuse_join_pattern: bool,
    pub enable_merge_vector_thresholds: bool,
    pub enable_reorder_intersect: bool,
    pub enable_reorder_fusion_signals: bool,
    pub enable_apply_index_scan: bool,
}

/// Query-local physical index candidate supplied by an engine catalog.
///
/// The candidate already contains the scan cost for this predicate. This
/// keeps the planner independent of an engine's index implementation while
/// allowing the final optimizer pass to emit a concrete
/// [`OperatorTree::IndexScan`].
#[derive(Debug, Clone, PartialEq)]
pub struct IndexScanCandidate {
    pub index_name: String,
    pub table_name: String,
    pub field: String,
    pub predicate: Predicate,
    pub scan_cost: f64,
}

impl Default for OptimizerConfig {
    fn default() -> Self {
        Self {
            enable_simplify_algebra: true,
            enable_push_filters_down: true,
            enable_push_graph_pattern_filters: true,
            enable_push_filter_into_traverse: true,
            enable_push_filter_below_graph_join: true,
            enable_fuse_join_pattern: true,
            enable_merge_vector_thresholds: true,
            enable_reorder_intersect: true,
            enable_reorder_fusion_signals: true,
            enable_apply_index_scan: true,
        }
    }
}

/// Operator-tree query optimizer.
pub struct QueryOptimizer {
    pub estimator: CardinalityEstimator,
    pub cost_estimator: CostEstimator,
    pub cost_model: CostModel,
    pub graph_stats: Option<GraphStats>,
    pub index_manager: Option<Arc<IndexManager>>,
    pub index_candidates: Vec<IndexScanCandidate>,
    pub table_name: Option<String>,
    pub row_count: Option<u64>,
    pub config: OptimizerConfig,
}

impl QueryOptimizer {
    pub fn new() -> Self {
        Self {
            estimator: CardinalityEstimator::new(),
            cost_estimator: CostEstimator::default(),
            cost_model: CostModel::new(),
            graph_stats: None,
            index_manager: None,
            index_candidates: Vec::new(),
            table_name: None,
            row_count: None,
            config: OptimizerConfig::default(),
        }
    }

    pub fn with_index_manager(mut self, im: Arc<IndexManager>, table: impl Into<String>) -> Self {
        self.index_manager = Some(im);
        self.table_name = Some(table.into());
        self
    }

    /// Attach candidates discovered from an engine's physical catalog.
    /// They compete with an optional storage [`IndexManager`] by scan cost.
    pub fn with_index_candidates(
        mut self,
        candidates: impl IntoIterator<Item = IndexScanCandidate>,
        table: impl Into<String>,
    ) -> Self {
        self.index_candidates = candidates.into_iter().collect();
        self.table_name = Some(table.into());
        self
    }

    pub fn with_graph_stats(mut self, gs: GraphStats) -> Self {
        self.cost_model = CostModel::new().with_graph_stats(gs.clone());
        self.estimator = std::mem::take(&mut self.estimator).with_graph_stats(gs.clone());
        self.graph_stats = Some(gs);
        self
    }

    pub fn with_row_count(mut self, n: u64) -> Self {
        self.row_count = Some(n);
        self
    }

    /// Top-level entry point. Mirrors `QueryOptimizer.optimize`.
    pub fn optimize(&self, op: OperatorTree) -> OperatorTree {
        let mut op = op;
        if self.config.enable_simplify_algebra {
            op = self.simplify_algebra(op);
        }
        if self.config.enable_push_filters_down {
            op = self.push_filters_down(op);
        }
        if self.config.enable_push_graph_pattern_filters {
            op = self.push_graph_pattern_filters(op);
        }
        if self.config.enable_push_filter_into_traverse {
            op = self.push_filter_into_traverse(op);
        }
        if self.config.enable_push_filter_below_graph_join {
            op = self.push_filter_below_graph_join(op);
        }
        if self.config.enable_fuse_join_pattern {
            op = Self::fuse_join_pattern(op);
        }
        if self.config.enable_merge_vector_thresholds {
            op = self.merge_vector_thresholds(op);
        }
        if self.config.enable_reorder_intersect {
            op = self.reorder_intersect(op);
        }
        if self.config.enable_reorder_fusion_signals {
            op = self.reorder_fusion_signals(op);
        }
        if self.config.enable_apply_index_scan {
            op = self.apply_index_scan(op);
        }
        op
    }
}

impl Default for QueryOptimizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
