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

#![allow(clippy::too_many_lines)]

use std::sync::Arc;

use uqa_core::Predicate;
use uqa_operators::{
    DeepFusionLayer, EdgePatternIR, GraphPatternIR, MultiStageEntry, OperatorTree, ProbBoolMode,
    ProgressiveFusionEntry, VertexPatternIR, VertexPredicate,
};
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

    // ---------------------------------------------------------------
    // 1. Algebraic simplification
    // ---------------------------------------------------------------

    fn simplify_algebra(&self, op: OperatorTree) -> OperatorTree {
        // Recurse first (bottom-up).
        let op = self.recurse_simplify(op);
        match op {
            OperatorTree::Intersect(operands) => {
                // Empty elimination: any empty child collapses the
                // intersection.
                for child in &operands {
                    if child.is_empty() {
                        return OperatorTree::Intersect(Vec::new());
                    }
                }
                // Idempotence is valid only for membership-only operands.
                // Posting-list merges add scores, so structurally equal scored
                // terms must remain distinct.
                let mut seen: Vec<OperatorTree> = Vec::new();
                'outer: for child in operands {
                    for s in &seen {
                        if same_membership_term(s, &child) {
                            continue 'outer;
                        }
                    }
                    seen.push(child);
                }
                let operands = seen;
                // Absorption: drop Union(A, ...) when A also appears
                // in the intersection.
                let mut absorbed: Vec<OperatorTree> = Vec::new();
                for (child_index, child) in operands.iter().enumerate() {
                    if let OperatorTree::Union(union_operands) = child {
                        let drop = is_membership_only(child)
                            && operands.iter().enumerate().any(|(other_index, other)| {
                                other_index != child_index
                                    && union_operands
                                        .iter()
                                        .any(|union_term| same_membership_term(union_term, other))
                            });
                        if drop {
                            continue;
                        }
                    }
                    absorbed.push(child.clone());
                }
                if absorbed.len() == 1 {
                    if let Some(only) = absorbed.pop() {
                        return only;
                    }
                }
                OperatorTree::Intersect(absorbed)
            }
            OperatorTree::Union(operands) => {
                // Drop empty children.
                let mut kept: Vec<OperatorTree> =
                    operands.into_iter().filter(|c| !c.is_empty()).collect();
                // Idempotence is valid only for membership-only operands.
                let mut seen: Vec<OperatorTree> = Vec::new();
                'outer: for child in kept.drain(..) {
                    for s in &seen {
                        if same_membership_term(s, &child) {
                            continue 'outer;
                        }
                    }
                    seen.push(child);
                }
                let operands = seen;
                // Absorption: drop Intersect(A, ...) when A also appears
                // in the union.
                let mut absorbed: Vec<OperatorTree> = Vec::new();
                for (child_index, child) in operands.iter().enumerate() {
                    if let OperatorTree::Intersect(int_operands) = child {
                        let drop = is_membership_only(child)
                            && operands.iter().enumerate().any(|(other_index, other)| {
                                other_index != child_index
                                    && int_operands.iter().any(|intersect_term| {
                                        same_membership_term(intersect_term, other)
                                    })
                            });
                        if drop {
                            continue;
                        }
                    }
                    absorbed.push(child.clone());
                }
                if absorbed.len() == 1 {
                    if let Some(only) = absorbed.pop() {
                        return only;
                    }
                }
                if absorbed.is_empty() {
                    return OperatorTree::Union(Vec::new());
                }
                OperatorTree::Union(absorbed)
            }
            other => other,
        }
    }

    fn recurse_simplify(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.simplify_algebra(child))
    }

    // ---------------------------------------------------------------
    // 2. Filter pushdown into Intersect
    // ---------------------------------------------------------------

    fn push_filters_down(&self, op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Filter {
            field,
            predicate,
            source: Some(s),
        } = op
        {
            if let OperatorTree::Intersect(operands) = *s {
                let mut new_operands: Vec<OperatorTree> = Vec::with_capacity(operands.len());
                let mut any_pushed = false;
                for child in operands {
                    if Self::filter_applies_to(&field, &child) {
                        new_operands.push(OperatorTree::Filter {
                            field: field.clone(),
                            predicate: predicate.clone(),
                            source: Some(Box::new(child)),
                        });
                        any_pushed = true;
                    } else {
                        new_operands.push(child);
                    }
                }
                if any_pushed {
                    let recursed: Vec<OperatorTree> = new_operands
                        .into_iter()
                        .map(|o| self.push_filters_down(o))
                        .collect();
                    return self.recurse_children(OperatorTree::Intersect(recursed));
                }
                // No push happened; rebuild the original Filter.
                return OperatorTree::Filter {
                    field,
                    predicate,
                    source: Some(Box::new(
                        self.recurse_children(OperatorTree::Intersect(new_operands)),
                    )),
                };
            }
            // Source is something else; just recurse through it.
            return OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.recurse_children(*s))),
            };
        }
        self.recurse_children(op)
    }

    // ---------------------------------------------------------------
    // 3. Push graph pattern filters
    // ---------------------------------------------------------------

    fn push_graph_pattern_filters(&self, op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Filter {
            field,
            predicate,
            source: Some(s),
        } = &op
        {
            if let OperatorTree::PatternMatch { pattern, graph } = s.as_ref() {
                let parts: Vec<&str> = field.splitn(2, '.').collect();
                if parts.len() == 2 {
                    let target_var = parts[0];
                    let prop = parts[1].to_string();
                    let mut new_vps = Vec::with_capacity(pattern.vertex_patterns.len());
                    let mut pushed = false;
                    for vp in &pattern.vertex_patterns {
                        if !pushed && vp.variable == target_var {
                            let mut constraints = vp.constraints.clone();
                            let pred = predicate.clone();
                            let prop_clone = prop.clone();
                            constraints.push(Arc::new(move |v: &uqa_core::Vertex| {
                                v.properties
                                    .get(&prop_clone)
                                    .is_some_and(|val| pred.evaluate(Some(val)))
                            }));
                            new_vps.push(VertexPatternIR {
                                variable: vp.variable.clone(),
                                constraints,
                                label: vp.label.clone(),
                            });
                            pushed = true;
                        } else {
                            new_vps.push(vp.clone());
                        }
                    }
                    if pushed {
                        let new_pattern = GraphPatternIR {
                            vertex_patterns: new_vps,
                            edge_patterns: pattern.edge_patterns.clone(),
                        };
                        return OperatorTree::PatternMatch {
                            pattern: new_pattern,
                            graph: graph.clone(),
                        };
                    }
                    // Try edge variable: "src_tgt.prop". Mirror the canonical UQA implementation's
                    // two-stage match: first build the
                    // `source_target -> ep` lookup (last-write-wins so
                    // multi-label edges between the same vertices fall
                    // through to the most recently declared label),
                    // then push the new constraint onto *every* edge
                    // sharing the chosen triple `(source, target, label)`.
                    let mut edge_lookup: std::collections::BTreeMap<String, EdgePatternIR> =
                        std::collections::BTreeMap::new();
                    for ep in &pattern.edge_patterns {
                        let key = format!("{}_{}", ep.source_var, ep.target_var);
                        edge_lookup.insert(key, ep.clone());
                    }
                    let chosen = edge_lookup.get(target_var).cloned();
                    let mut new_eps = Vec::with_capacity(pattern.edge_patterns.len());
                    if let Some(ref chosen_ep) = chosen {
                        for orig_ep in &pattern.edge_patterns {
                            if orig_ep.source_var == chosen_ep.source_var
                                && orig_ep.target_var == chosen_ep.target_var
                                && orig_ep.label == chosen_ep.label
                            {
                                let mut constraints = orig_ep.constraints.clone();
                                let pred = predicate.clone();
                                let prop_clone = prop.clone();
                                constraints.push(Arc::new(move |e: &uqa_core::Edge| {
                                    e.properties
                                        .get(&prop_clone)
                                        .is_some_and(|val| pred.evaluate(Some(val)))
                                }));
                                new_eps.push(EdgePatternIR {
                                    source_var: orig_ep.source_var.clone(),
                                    target_var: orig_ep.target_var.clone(),
                                    label: orig_ep.label.clone(),
                                    constraints,
                                });
                                pushed = true;
                            } else {
                                new_eps.push(orig_ep.clone());
                            }
                        }
                    }
                    if pushed {
                        let new_pattern = GraphPatternIR {
                            vertex_patterns: pattern.vertex_patterns.clone(),
                            edge_patterns: new_eps,
                        };
                        return OperatorTree::PatternMatch {
                            pattern: new_pattern,
                            graph: graph.clone(),
                        };
                    }
                }
                // Cannot push; keep the filter wrapped.
                return OperatorTree::Filter {
                    field: field.clone(),
                    predicate: predicate.clone(),
                    source: Some(Box::new(s.as_ref().clone())),
                };
            }
        }
        self.recurse_graph_pattern(op)
    }

    fn recurse_graph_pattern(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.push_graph_pattern_filters(child))
    }

    // ---------------------------------------------------------------
    // 4. Push filter into Traverse
    // ---------------------------------------------------------------

    fn push_filter_into_traverse(&self, op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Filter {
            field,
            predicate,
            source: Some(s),
        } = &op
        {
            if let OperatorTree::Traverse {
                start_vertex,
                graph,
                label,
                max_hops,
                vertex_predicate,
            } = s.as_ref()
            {
                let pred = predicate.clone();
                let field_clone = field.clone();
                let leaf_filter: VertexPredicate = Arc::new(move |v: &uqa_core::Vertex| {
                    v.properties
                        .get(&field_clone)
                        .is_some_and(|val| pred.evaluate(Some(val)))
                });
                let combined: VertexPredicate = match vertex_predicate {
                    Some(prev) => {
                        let prev = prev.clone();
                        Arc::new(move |v: &uqa_core::Vertex| prev(v) && leaf_filter(v))
                    }
                    None => leaf_filter,
                };
                return OperatorTree::Traverse {
                    start_vertex: *start_vertex,
                    graph: graph.clone(),
                    label: label.clone(),
                    max_hops: *max_hops,
                    vertex_predicate: Some(combined),
                };
            }
        }
        self.recurse_traverse_filter(op)
    }

    fn recurse_traverse_filter(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.push_filter_into_traverse(child))
    }

    // ---------------------------------------------------------------
    // 5. Push filter below GraphJoin
    // ---------------------------------------------------------------

    fn push_filter_below_graph_join(&self, op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Filter {
            field,
            predicate,
            source: Some(s),
        } = op
        {
            if let OperatorTree::GraphJoin {
                left,
                right,
                label,
                graph,
            } = *s
            {
                let new_left = OperatorTree::Filter {
                    field: field.clone(),
                    predicate: predicate.clone(),
                    source: Some(left),
                };
                return OperatorTree::GraphJoin {
                    left: Box::new(new_left),
                    right,
                    label,
                    graph,
                };
            }
            return OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.push_filter_below_graph_join(*s))),
            };
        }
        self.recurse_graph_join(op)
    }

    fn recurse_graph_join(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.push_filter_below_graph_join(child))
    }

    // ---------------------------------------------------------------
    // 6. Fuse PatternMatch operators inside an Intersect
    // ---------------------------------------------------------------

    fn fuse_join_pattern(op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Intersect(ops) = op {
            let children: Vec<OperatorTree> =
                ops.into_iter().map(Self::fuse_join_pattern).collect();
            let mut pattern_ops: Vec<OperatorTree> = Vec::new();
            let mut other_ops: Vec<OperatorTree> = Vec::new();
            for child in children {
                if matches!(child, OperatorTree::PatternMatch { .. }) {
                    pattern_ops.push(child);
                } else {
                    other_ops.push(child);
                }
            }
            // Pairwise fuse PatternMatch operators that share a vertex
            // variable.
            if pattern_ops.len() >= 2 {
                let mut merged: Vec<OperatorTree> = Vec::with_capacity(pattern_ops.len());
                merged.push(pattern_ops.remove(0));
                for pm in pattern_ops {
                    let Some(last) = merged.pop() else {
                        merged.push(pm);
                        continue;
                    };
                    match Self::merge_patterns(&last, &pm) {
                        Some(fused) => merged.push(fused),
                        None => {
                            merged.push(last);
                            merged.push(pm);
                        }
                    }
                }
                pattern_ops = merged;
            }
            let mut all = other_ops;
            all.extend(pattern_ops);
            if all.len() == 1 {
                if let Some(only) = all.pop() {
                    return only;
                }
            }
            return OperatorTree::Intersect(all);
        }
        map_operator_children(op, Self::fuse_join_pattern)
    }

    fn merge_patterns(a: &OperatorTree, b: &OperatorTree) -> Option<OperatorTree> {
        let (
            OperatorTree::PatternMatch {
                pattern: pa,
                graph: ga,
            },
            OperatorTree::PatternMatch {
                pattern: pb,
                graph: _gb,
            },
        ) = (a, b)
        else {
            return None;
        };
        let vars_a: std::collections::BTreeSet<&String> =
            pa.vertex_patterns.iter().map(|vp| &vp.variable).collect();
        let vars_b: std::collections::BTreeSet<&String> =
            pb.vertex_patterns.iter().map(|vp| &vp.variable).collect();
        if vars_a.intersection(&vars_b).next().is_none() {
            return None;
        }
        // The merge buffer for the merge buffer, which keeps
        // insertion order. Mirror that with a parallel `Vec` (positions)
        // + `BTreeMap` (lookup) so structurally-equivalent inputs give
        // structurally-equivalent merged patterns regardless of name
        // collation.
        let mut order: Vec<String> = Vec::new();
        let mut by_var: std::collections::BTreeMap<String, VertexPatternIR> =
            std::collections::BTreeMap::new();
        for vp in &pa.vertex_patterns {
            order.push(vp.variable.clone());
            by_var.insert(vp.variable.clone(), vp.clone());
        }
        for vp in &pb.vertex_patterns {
            match by_var.get_mut(&vp.variable) {
                Some(existing) => {
                    let mut combined = existing.constraints.clone();
                    combined.extend(vp.constraints.iter().cloned());
                    existing.constraints = combined;
                }
                None => {
                    order.push(vp.variable.clone());
                    by_var.insert(vp.variable.clone(), vp.clone());
                }
            }
        }
        let merged_vps: Vec<VertexPatternIR> = order
            .into_iter()
            .filter_map(|var| by_var.remove(&var))
            .collect();
        let mut edge_patterns = pa.edge_patterns.clone();
        edge_patterns.extend(pb.edge_patterns.iter().cloned());
        let new_pattern = GraphPatternIR {
            vertex_patterns: merged_vps,
            edge_patterns,
        };
        Some(OperatorTree::PatternMatch {
            pattern: new_pattern,
            graph: ga.clone(),
        })
    }

    // ---------------------------------------------------------------
    // 7. Merge adjacent vector thresholds
    // ---------------------------------------------------------------

    fn merge_vector_thresholds(&self, op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Intersect(operands) = op {
            let mut vector_ops: Vec<(Vec<f32>, f32, String)> = Vec::new();
            let mut other_ops: Vec<OperatorTree> = Vec::new();
            for child in operands {
                let child = self.recurse_children(child);
                match child {
                    OperatorTree::VectorSimilarity {
                        query_vector,
                        threshold,
                        field,
                    } => vector_ops.push((query_vector, threshold, field)),
                    other => other_ops.push(other),
                }
            }
            let mut merged_vectors: Vec<OperatorTree> = Vec::new();
            let mut used = vec![false; vector_ops.len()];
            for i in 0..vector_ops.len() {
                if used[i] {
                    continue;
                }
                let (q, mut t, f) = (
                    vector_ops[i].0.clone(),
                    vector_ops[i].1,
                    vector_ops[i].2.clone(),
                );
                for j in (i + 1)..vector_ops.len() {
                    if used[j] {
                        continue;
                    }
                    if vector_ops[j].2 == f && vectors_close(&q, &vector_ops[j].0) {
                        t = t.max(vector_ops[j].1);
                        used[j] = true;
                    }
                }
                used[i] = true;
                merged_vectors.push(OperatorTree::VectorSimilarity {
                    query_vector: q,
                    threshold: t,
                    field: f,
                });
            }
            let mut all = other_ops;
            all.extend(merged_vectors);
            if all.len() == 1 {
                if let Some(only) = all.pop() {
                    return only;
                }
            }
            return OperatorTree::Intersect(all);
        }
        self.recurse_children(op)
    }

    // ---------------------------------------------------------------
    // 8. Reorder Intersect by estimated cardinality
    // ---------------------------------------------------------------

    fn reorder_intersect(&self, op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Intersect(operands) = op {
            let mut children: Vec<OperatorTree> = operands
                .into_iter()
                .map(|c| self.recurse_children(c))
                .collect();
            // Match UQA behavior for: the optimizer ranks intersect arms by the
            // algebraic operator cost (`CostModel.estimate`), not the
            // cardinality estimator. The two diverge for `Filter`,
            // `Score`, `Traverse`, `RegularPathQuery`, fusion / hybrid
            // / cross-paradigm join nodes, and any operator with a
            // dedicated formula in `cost_model`.
            let cost_stats = uqa_core::IndexStats::new(self.row_count.unwrap_or(1_000));
            children.sort_by(|a, b| {
                let ca = self.cost_model.estimate(a, &cost_stats);
                let cb = self.cost_model.estimate(b, &cost_stats);
                ca.total_cmp(&cb)
            });
            return OperatorTree::Intersect(children);
        }
        self.recurse_children(op)
    }

    // ---------------------------------------------------------------
    // 9. Reorder fusion signals
    // ---------------------------------------------------------------

    fn reorder_fusion_signals(&self, op: OperatorTree) -> OperatorTree {
        match op {
            OperatorTree::BayesianEvidenceFusion { signals, base_rate } => {
                let mut signals: Vec<_> = signals
                    .into_iter()
                    .map(|signal| self.reorder_fusion_signals(signal))
                    .collect();
                signals.sort_by(|left, right| {
                    self.graph_aware_signal_cost(left)
                        .total_cmp(&self.graph_aware_signal_cost(right))
                });
                OperatorTree::BayesianEvidenceFusion { signals, base_rate }
            }
            OperatorTree::RobustPositiveEvidencePool {
                signals,
                alpha,
                gating,
                weights,
                logit_min,
                logit_max,
                adaptive_weights,
            } => {
                let mut indexed_signals: Vec<(usize, OperatorTree)> = signals
                    .into_iter()
                    .enumerate()
                    .map(|(index, signal)| (index, self.reorder_fusion_signals(signal)))
                    .collect();
                indexed_signals.sort_by(|(_, left), (_, right)| {
                    let ca = self.graph_aware_signal_cost(left);
                    let cb = self.graph_aware_signal_cost(right);
                    ca.total_cmp(&cb)
                });
                let order: Vec<usize> = indexed_signals
                    .iter()
                    .map(|(original_index, _)| *original_index)
                    .collect();
                let reordered_weights =
                    weights.map(|values| order.iter().map(|index| values[*index]).collect());
                let reordered_logit_min =
                    logit_min.map(|values| order.iter().map(|index| values[*index]).collect());
                let reordered_logit_max =
                    logit_max.map(|values| order.iter().map(|index| values[*index]).collect());
                OperatorTree::RobustPositiveEvidencePool {
                    signals: indexed_signals
                        .into_iter()
                        .map(|(_, signal)| signal)
                        .collect(),
                    alpha,
                    gating,
                    weights: reordered_weights,
                    logit_min: reordered_logit_min,
                    logit_max: reordered_logit_max,
                    adaptive_weights,
                }
            }
            OperatorTree::ProbBoolFusion { signals, mode } => {
                let mut sigs: Vec<OperatorTree> = signals
                    .into_iter()
                    .map(|s| self.reorder_fusion_signals(s))
                    .collect();
                sigs.sort_by(|a, b| {
                    let ca = self.graph_aware_signal_cost(a);
                    let cb = self.graph_aware_signal_cost(b);
                    ca.total_cmp(&cb)
                });
                OperatorTree::ProbBoolFusion {
                    signals: sigs,
                    mode,
                }
            }
            other => self.recurse_fusion(other),
        }
    }

    fn graph_aware_signal_cost(&self, signal: &OperatorTree) -> f64 {
        let base = self.estimator.estimate_operator(signal, self.row_count);
        if self.graph_stats.is_some()
            && matches!(
                signal,
                OperatorTree::Traverse { .. }
                    | OperatorTree::PatternMatch { .. }
                    | OperatorTree::RegularPathQuery { .. }
            )
        {
            base * 0.5
        } else {
            base
        }
    }

    fn recurse_fusion(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.reorder_fusion_signals(child))
    }

    // ---------------------------------------------------------------
    // 10. Substitute leaf Filter with IndexScan
    // ---------------------------------------------------------------

    fn apply_index_scan(&self, op: OperatorTree) -> OperatorTree {
        let Some(table) = &self.table_name else {
            return self.recurse_index_scan(op);
        };
        if let OperatorTree::Filter {
            field,
            predicate,
            source: None,
        } = &op
        {
            let managed = self
                .index_manager
                .as_ref()
                .and_then(|manager| manager.find_covering_index_with_cost(table, field, predicate));
            let catalog = self
                .index_candidates
                .iter()
                .filter(|candidate| {
                    candidate.table_name == *table
                        && candidate.field == *field
                        && candidate.predicate == *predicate
                        && candidate.scan_cost.is_finite()
                        && candidate.scan_cost >= 0.0
                })
                .map(|candidate| (candidate.index_name.clone(), candidate.scan_cost))
                .min_by(|left, right| left.1.total_cmp(&right.1));
            let best = match (managed, catalog) {
                (Some(left), Some(right)) if left.1 <= right.1 => Some(left),
                (Some(_), Some(right)) => Some(right),
                (Some(candidate), None) | (None, Some(candidate)) => Some(candidate),
                (None, None) => None,
            };
            if let Some((name, scan_cost)) = best {
                // the canonical UQA implementation's `_apply_index_scan` only rewrites when the
                // index's `scan_cost(predicate)` beats a full scan.
                // Mirror that gate exactly: prefer the index only when
                // its cost is strictly cheaper.
                let full_scan_cost = self.row_count.unwrap_or(0) as f64;
                if scan_cost < full_scan_cost {
                    return OperatorTree::IndexScan {
                        index_name: name,
                        field: field.clone(),
                        predicate: predicate.clone(),
                    };
                }
            }
        }
        if let OperatorTree::Filter {
            field,
            predicate,
            source: Some(s),
        } = op
        {
            return OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.apply_index_scan(*s))),
            };
        }
        self.recurse_index_scan(op)
    }

    fn recurse_index_scan(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.apply_index_scan(child))
    }

    // ---------------------------------------------------------------
    // Generic recursion (used by simplify / merge_vector / reorder)
    // ---------------------------------------------------------------

    fn recurse_children(&self, op: OperatorTree) -> OperatorTree {
        map_operator_children(op, |child| self.optimize(child))
    }

    fn filter_applies_to(field: &str, target: &OperatorTree) -> bool {
        match target {
            OperatorTree::Term {
                field: term_field, ..
            } => match term_field {
                Some(f) => f == field,
                None => true,
            },
            OperatorTree::Filter {
                field: filter_field,
                ..
            } => filter_field == field,
            OperatorTree::Intersect(ops) => ops.iter().any(|c| Self::filter_applies_to(field, c)),
            _ => false,
        }
    }
}

/// Apply one rewrite function to every direct child carried by the IR.
/// Keeping this structural traversal exhaustive prevents a newly
/// executable wrapper (join, graph embedding, progressive/deep fusion,
/// and so on) from becoming an optimizer boundary by accident.
fn map_operator_children(
    op: OperatorTree,
    mut map: impl FnMut(OperatorTree) -> OperatorTree,
) -> OperatorTree {
    match op {
        OperatorTree::Filter {
            field,
            predicate,
            source,
        } => OperatorTree::Filter {
            field,
            predicate,
            source: source.map(|child| Box::new(map(*child))),
        },
        OperatorTree::Facet { field, source } => OperatorTree::Facet {
            field,
            source: source.map(|child| Box::new(map(*child))),
        },
        OperatorTree::Score {
            scorer,
            source,
            query_terms,
            field,
        } => OperatorTree::Score {
            scorer,
            source: Box::new(map(*source)),
            query_terms,
            field,
        },
        OperatorTree::BayesianScore { source, field } => OperatorTree::BayesianScore {
            source: Box::new(map(*source)),
            field,
        },
        OperatorTree::Intersect(children) => {
            OperatorTree::Intersect(children.into_iter().map(&mut map).collect())
        }
        OperatorTree::Union(children) => {
            OperatorTree::Union(children.into_iter().map(&mut map).collect())
        }
        OperatorTree::Complement(child) => OperatorTree::Complement(Box::new(map(*child))),
        OperatorTree::Composed(children) => {
            OperatorTree::Composed(children.into_iter().map(&mut map).collect())
        }
        OperatorTree::CosineProbability(child) => {
            OperatorTree::CosineProbability(Box::new(map(*child)))
        }
        OperatorTree::BayesianEvidenceFusion { signals, base_rate } => {
            OperatorTree::BayesianEvidenceFusion {
                signals: signals.into_iter().map(&mut map).collect(),
                base_rate,
            }
        }
        OperatorTree::RobustPositiveEvidencePool {
            signals,
            alpha,
            gating,
            weights,
            logit_min,
            logit_max,
            adaptive_weights,
        } => OperatorTree::RobustPositiveEvidencePool {
            signals: signals.into_iter().map(&mut map).collect(),
            alpha,
            gating,
            weights,
            logit_min,
            logit_max,
            adaptive_weights,
        },
        OperatorTree::ProbBoolFusion { signals, mode } => OperatorTree::ProbBoolFusion {
            signals: signals.into_iter().map(&mut map).collect(),
            mode,
        },
        OperatorTree::ProbNot {
            signal,
            default_prob,
        } => OperatorTree::ProbNot {
            signal: Box::new(map(*signal)),
            default_prob,
        },
        OperatorTree::AttentionFusion {
            signals,
            attention,
            query_features,
        } => OperatorTree::AttentionFusion {
            signals: signals.into_iter().map(&mut map).collect(),
            attention,
            query_features,
        },
        OperatorTree::LearnedFusion { signals, learned } => OperatorTree::LearnedFusion {
            signals: signals.into_iter().map(&mut map).collect(),
            learned,
        },
        OperatorTree::SparseThreshold { source, threshold } => OperatorTree::SparseThreshold {
            source: Box::new(map(*source)),
            threshold,
        },
        OperatorTree::GraphJoin {
            left,
            right,
            label,
            graph,
        } => OperatorTree::GraphJoin {
            left: Box::new(map(*left)),
            right: Box::new(map(*right)),
            label,
            graph,
        },
        OperatorTree::Aggregate {
            source,
            field,
            monoid,
        } => OperatorTree::Aggregate {
            source: source.map(|child| Box::new(map(*child))),
            field,
            monoid,
        },
        OperatorTree::GroupBy {
            source,
            group_field,
            agg_field,
            monoid,
        } => OperatorTree::GroupBy {
            source: Box::new(map(*source)),
            group_field,
            agg_field,
            monoid,
        },
        OperatorTree::MultiStage { stages } => OperatorTree::MultiStage {
            stages: stages
                .into_iter()
                .map(|stage| MultiStageEntry {
                    child: map(stage.child),
                    cutoff: stage.cutoff,
                })
                .collect(),
        },
        OperatorTree::HybridTextVector {
            term_op,
            vector_op,
            alpha,
        } => OperatorTree::HybridTextVector {
            term_op: Box::new(map(*term_op)),
            vector_op: Box::new(map(*vector_op)),
            alpha,
        },
        OperatorTree::SemanticFilter { source, vector_op } => OperatorTree::SemanticFilter {
            source: Box::new(map(*source)),
            vector_op: Box::new(map(*vector_op)),
        },
        OperatorTree::VectorExclusion { positive, negative } => OperatorTree::VectorExclusion {
            positive: Box::new(map(*positive)),
            negative: Box::new(map(*negative)),
        },
        OperatorTree::FacetVector {
            vector_op,
            facet_field,
        } => OperatorTree::FacetVector {
            vector_op: Box::new(map(*vector_op)),
            facet_field,
        },
        OperatorTree::VertexAggregation { source, monoid } => OperatorTree::VertexAggregation {
            source: Box::new(map(*source)),
            monoid,
        },
        OperatorTree::MessagePassing { source } => OperatorTree::MessagePassing {
            source: Box::new(map(*source)),
        },
        OperatorTree::GraphEmbedding { source } => OperatorTree::GraphEmbedding {
            source: Box::new(map(*source)),
        },
        OperatorTree::TextSimilarityJoin {
            left,
            right,
            threshold,
        } => OperatorTree::TextSimilarityJoin {
            left: Box::new(map(*left)),
            right: Box::new(map(*right)),
            threshold,
        },
        OperatorTree::VectorSimilarityJoin {
            left,
            right,
            threshold,
        } => OperatorTree::VectorSimilarityJoin {
            left: Box::new(map(*left)),
            right: Box::new(map(*right)),
            threshold,
        },
        OperatorTree::HybridJoin { left, right } => OperatorTree::HybridJoin {
            left: Box::new(map(*left)),
            right: Box::new(map(*right)),
        },
        OperatorTree::CrossParadigmJoin { left, right } => OperatorTree::CrossParadigmJoin {
            left: Box::new(map(*left)),
            right: Box::new(map(*right)),
        },
        OperatorTree::ProgressiveFusion {
            stages,
            alpha,
            gating,
        } => OperatorTree::ProgressiveFusion {
            stages: stages
                .into_iter()
                .map(|stage| ProgressiveFusionEntry {
                    signal: map(stage.signal),
                    k: stage.k,
                })
                .collect(),
            alpha,
            gating,
        },
        OperatorTree::DeepFusion {
            layers,
            alpha,
            gating,
        } => OperatorTree::DeepFusion {
            layers: layers
                .into_iter()
                .map(|layer| match layer {
                    DeepFusionLayer::Signal { signals } => DeepFusionLayer::Signal {
                        signals: signals.into_iter().map(&mut map).collect(),
                    },
                    other => other,
                })
                .collect(),
            alpha,
            gating,
        },
        OperatorTree::Opaque {
            kind,
            children,
            meta,
        } => OperatorTree::Opaque {
            kind,
            children: children.into_iter().map(&mut map).collect(),
            meta,
        },
        OperatorTree::Empty => OperatorTree::Empty,
        OperatorTree::Term {
            query,
            field,
            scoring,
        } => OperatorTree::Term {
            query,
            field,
            scoring,
        },
        OperatorTree::BayesianMatchWithPrior {
            field,
            query,
            prior_field,
            mode,
        } => OperatorTree::BayesianMatchWithPrior {
            field,
            query,
            prior_field,
            mode,
        },
        OperatorTree::VectorSimilarity {
            query_vector,
            threshold,
            field,
        } => OperatorTree::VectorSimilarity {
            query_vector,
            threshold,
            field,
        },
        OperatorTree::KNN {
            query_vector,
            k,
            field,
        } => OperatorTree::KNN {
            query_vector,
            k,
            field,
        },
        OperatorTree::CalibratedVectorMatch {
            query_vector,
            k,
            field,
            threshold,
        } => OperatorTree::CalibratedVectorMatch {
            query_vector,
            k,
            field,
            threshold,
        },
        OperatorTree::Traverse {
            start_vertex,
            graph,
            label,
            max_hops,
            vertex_predicate,
        } => OperatorTree::Traverse {
            start_vertex,
            graph,
            label,
            max_hops,
            vertex_predicate,
        },
        OperatorTree::GraphNeighbors {
            vertex,
            graph,
            label,
            direction,
        } => OperatorTree::GraphNeighbors {
            vertex,
            graph,
            label,
            direction,
        },
        OperatorTree::GraphEdges { graph, label } => OperatorTree::GraphEdges { graph, label },
        OperatorTree::PatternMatch { pattern, graph } => {
            OperatorTree::PatternMatch { pattern, graph }
        }
        OperatorTree::RegularPathQuery {
            rpq_source,
            start_vertex,
            graph,
        } => OperatorTree::RegularPathQuery {
            rpq_source,
            start_vertex,
            graph,
        },
        OperatorTree::IndexScan {
            index_name,
            field,
            predicate,
        } => OperatorTree::IndexScan {
            index_name,
            field,
            predicate,
        },
        OperatorTree::MultiFieldSearch {
            fields,
            queries,
            weights,
        } => OperatorTree::MultiFieldSearch {
            fields,
            queries,
            weights,
        },
        OperatorTree::WeightedPathQuery {
            rpq_source,
            start_vertex,
            graph,
            weight_property,
            default_edge_weight,
            max_hops,
            predicate,
            predicate_selectivity,
            score,
        } => OperatorTree::WeightedPathQuery {
            rpq_source,
            start_vertex,
            graph,
            weight_property,
            default_edge_weight,
            max_hops,
            predicate,
            predicate_selectivity,
            score,
        },
        OperatorTree::PageRank { graph } => OperatorTree::PageRank { graph },
        OperatorTree::HITS { graph } => OperatorTree::HITS { graph },
        OperatorTree::BetweennessCentrality { graph } => {
            OperatorTree::BetweennessCentrality { graph }
        }
        OperatorTree::TemporalTraverse {
            start_vertex,
            graph,
            label,
            max_hops,
            temporal_filter,
        } => OperatorTree::TemporalTraverse {
            start_vertex,
            graph,
            label,
            max_hops,
            temporal_filter,
        },
        OperatorTree::TemporalPatternMatch {
            pattern,
            graph,
            temporal_filter,
        } => OperatorTree::TemporalPatternMatch {
            pattern,
            graph,
            temporal_filter,
        },
        OperatorTree::DeepPredict { model } => OperatorTree::DeepPredict { model },
    }
}

/// Whether Boolean composition observes only membership for this subtree.
///
/// `PostingList::merge_union` and `PostingList::merge_intersection` add scores when the same
/// document appears on both sides. They may also carry operator-specific
/// fields. The algebraic identities are therefore safe only for the small,
/// explicit subset below, whose execution produces default payloads. Keeping
/// this match exhaustive makes a new `OperatorTree` variant opt out until its
/// payload effect has been reviewed.
fn is_membership_only(op: &OperatorTree) -> bool {
    match op {
        OperatorTree::Empty | OperatorTree::IndexScan { .. } => true,
        OperatorTree::Filter { source, .. } => source.as_deref().is_none_or(is_membership_only),
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children) => children.iter().all(is_membership_only),
        OperatorTree::Complement(child) => is_membership_only(child),
        OperatorTree::VectorExclusion { positive, negative } => {
            is_membership_only(positive) && is_membership_only(negative)
        }
        OperatorTree::Term { .. }
        | OperatorTree::Facet { .. }
        | OperatorTree::Score { .. }
        | OperatorTree::BayesianScore { .. }
        | OperatorTree::BayesianMatchWithPrior { .. }
        | OperatorTree::VectorSimilarity { .. }
        | OperatorTree::KNN { .. }
        | OperatorTree::CalibratedVectorMatch { .. }
        | OperatorTree::CosineProbability(_)
        | OperatorTree::BayesianEvidenceFusion { .. }
        | OperatorTree::RobustPositiveEvidencePool { .. }
        | OperatorTree::ProbBoolFusion { .. }
        | OperatorTree::ProbNot { .. }
        | OperatorTree::AttentionFusion { .. }
        | OperatorTree::LearnedFusion { .. }
        | OperatorTree::SparseThreshold { .. }
        | OperatorTree::Traverse { .. }
        | OperatorTree::GraphNeighbors { .. }
        | OperatorTree::GraphEdges { .. }
        | OperatorTree::PatternMatch { .. }
        | OperatorTree::RegularPathQuery { .. }
        | OperatorTree::GraphJoin { .. }
        | OperatorTree::Aggregate { .. }
        | OperatorTree::GroupBy { .. }
        | OperatorTree::MultiStage { .. }
        | OperatorTree::MultiFieldSearch { .. }
        | OperatorTree::HybridTextVector { .. }
        | OperatorTree::SemanticFilter { .. }
        | OperatorTree::FacetVector { .. }
        | OperatorTree::VertexAggregation { .. }
        | OperatorTree::WeightedPathQuery { .. }
        | OperatorTree::MessagePassing { .. }
        | OperatorTree::GraphEmbedding { .. }
        | OperatorTree::PageRank { .. }
        | OperatorTree::HITS { .. }
        | OperatorTree::BetweennessCentrality { .. }
        | OperatorTree::TextSimilarityJoin { .. }
        | OperatorTree::VectorSimilarityJoin { .. }
        | OperatorTree::HybridJoin { .. }
        | OperatorTree::CrossParadigmJoin { .. }
        | OperatorTree::TemporalTraverse { .. }
        | OperatorTree::TemporalPatternMatch { .. }
        | OperatorTree::ProgressiveFusion { .. }
        | OperatorTree::DeepFusion { .. }
        | OperatorTree::DeepPredict { .. }
        | OperatorTree::Opaque { .. } => false,
    }
}

/// Address-independent structural equivalence for operands on which Boolean
/// idempotence and absorption preserve the complete posting-list payload.
fn same_membership_term(left: &OperatorTree, right: &OperatorTree) -> bool {
    if !is_membership_only(left) || !is_membership_only(right) {
        return false;
    }

    // All three zero-child composition forms execute as the same empty set.
    if left.is_empty() || right.is_empty() {
        return left.is_empty() && right.is_empty();
    }

    match (left, right) {
        (
            OperatorTree::Filter {
                field: left_field,
                predicate: left_predicate,
                source: left_source,
            },
            OperatorTree::Filter {
                field: right_field,
                predicate: right_predicate,
                source: right_source,
            },
        ) => {
            left_field == right_field
                && left_predicate == right_predicate
                && same_optional_membership_source(left_source.as_deref(), right_source.as_deref())
        }
        (
            OperatorTree::IndexScan {
                index_name: left_index,
                field: left_field,
                predicate: left_predicate,
            },
            OperatorTree::IndexScan {
                index_name: right_index,
                field: right_field,
                predicate: right_predicate,
            },
        ) => {
            left_index == right_index
                && left_field == right_field
                && left_predicate == right_predicate
        }
        (OperatorTree::Intersect(left), OperatorTree::Intersect(right))
        | (OperatorTree::Union(left), OperatorTree::Union(right)) => {
            same_membership_multiset(left, right)
        }
        (OperatorTree::Complement(left), OperatorTree::Complement(right)) => {
            same_membership_term(left, right)
        }
        (OperatorTree::Composed(left), OperatorTree::Composed(right)) => {
            same_membership_sequence(left, right)
        }
        (
            OperatorTree::VectorExclusion {
                positive: left_positive,
                negative: left_negative,
            },
            OperatorTree::VectorExclusion {
                positive: right_positive,
                negative: right_negative,
            },
        ) => {
            same_membership_term(left_positive, right_positive)
                && same_membership_term(left_negative, right_negative)
        }
        _ => false,
    }
}

fn same_optional_membership_source(
    left: Option<&OperatorTree>,
    right: Option<&OperatorTree>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => same_membership_term(left, right),
        (None, Some(_)) | (Some(_), None) => false,
    }
}

fn same_membership_sequence(left: &[OperatorTree], right: &[OperatorTree]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| same_membership_term(left, right))
}

fn same_membership_multiset(left: &[OperatorTree], right: &[OperatorTree]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let mut matched = vec![false; right.len()];
    for left_term in left {
        let Some(index) = right.iter().enumerate().position(|(index, right_term)| {
            !matched[index] && same_membership_term(left_term, right_term)
        }) else {
            return false;
        };
        matched[index] = true;
    }
    true
}

impl Default for QueryOptimizer {
    fn default() -> Self {
        Self::new()
    }
}

fn vectors_close(a: &[f32], b: &[f32]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| (x - y).abs() <= 1e-7 * x.abs().max(y.abs()) + 1e-9)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(field: &str) -> OperatorTree {
        OperatorTree::Term {
            query: "q".into(),
            field: Some(field.into()),
            scoring: Some(uqa_operators::TextScoringMode::BM25),
        }
    }

    fn membership_filter(field: &str, value: i64) -> OperatorTree {
        OperatorTree::Filter {
            field: field.into(),
            predicate: Predicate::Equals(uqa_core::Value::Int(value)),
            source: None,
        }
    }

    #[test]
    fn empty_intersect_collapses_to_intersect_empty() {
        let op = OperatorTree::Intersect(vec![term("a"), OperatorTree::Intersect(vec![])]);
        let optimised = QueryOptimizer::new().optimize(op);
        assert!(optimised.is_empty());
    }

    #[test]
    fn empty_composition_has_the_same_empty_semantics_as_execution() {
        let empty_composition = OperatorTree::Composed(vec![]);
        assert!(empty_composition.is_empty());

        let intersection = QueryOptimizer::new().optimize(OperatorTree::Intersect(vec![
            term("a"),
            empty_composition.clone(),
        ]));
        assert!(intersection.is_empty());

        let union =
            QueryOptimizer::new().optimize(OperatorTree::Union(vec![term("a"), empty_composition]));
        assert!(matches!(union, OperatorTree::Term { .. }));
    }

    #[test]
    fn separately_allocated_membership_terms_are_idempotent() {
        let op = OperatorTree::Intersect(vec![
            membership_filter("year", 2026),
            membership_filter("year", 2026),
        ]);
        let optimised = QueryOptimizer::new().optimize(op);
        assert!(matches!(
            optimised,
            OperatorTree::Filter {
                ref field,
                predicate: Predicate::Equals(uqa_core::Value::Int(2026)),
                source: None,
            } if field == "year"
        ));
    }

    #[test]
    fn membership_absorption_uses_structural_equivalence() {
        let a = || membership_filter("year", 2026);
        let b = || membership_filter("year", 2025);

        let intersection = QueryOptimizer::new().optimize(OperatorTree::Intersect(vec![
            a(),
            OperatorTree::Union(vec![b(), a()]),
        ]));
        assert!(matches!(
            intersection,
            OperatorTree::Filter {
                predicate: Predicate::Equals(uqa_core::Value::Int(2026)),
                ..
            }
        ));

        let union = QueryOptimizer::new().optimize(OperatorTree::Union(vec![
            a(),
            OperatorTree::Intersect(vec![b(), a()]),
        ]));
        assert!(matches!(
            union,
            OperatorTree::Filter {
                predicate: Predicate::Equals(uqa_core::Value::Int(2026)),
                ..
            }
        ));
    }

    #[test]
    fn commutative_membership_subtrees_compare_independent_of_order() {
        let left = OperatorTree::Union(vec![
            membership_filter("year", 2025),
            membership_filter("year", 2026),
        ]);
        let right = OperatorTree::Union(vec![
            membership_filter("year", 2026),
            membership_filter("year", 2025),
        ]);

        let optimised = QueryOptimizer::new().optimize(OperatorTree::Intersect(vec![left, right]));
        let OperatorTree::Union(terms) = optimised else {
            panic!("expected one structurally deduplicated Union");
        };
        assert_eq!(terms.len(), 2);
    }

    #[test]
    fn structurally_distinct_membership_terms_remain_distinct() {
        let optimised = QueryOptimizer::new().optimize(OperatorTree::Intersect(vec![
            membership_filter("year", 2025),
            membership_filter("year", 2026),
        ]));
        let OperatorTree::Intersect(terms) = optimised else {
            panic!("expected distinct Intersect");
        };
        assert_eq!(terms.len(), 2);
    }

    #[test]
    fn scored_terms_keep_their_additive_effect() {
        let op = OperatorTree::Intersect(vec![term("a"), term("a")]);
        let optimised = QueryOptimizer::new().optimize(op);
        let OperatorTree::Intersect(terms) = optimised else {
            panic!("expected scored Intersect");
        };
        assert_eq!(terms.len(), 2);
    }

    #[test]
    fn absorption_does_not_discard_scored_branches() {
        let op = OperatorTree::Intersect(vec![
            term("a"),
            OperatorTree::Union(vec![term("b"), term("a")]),
        ]);
        let optimised = QueryOptimizer::new().optimize(op);
        let OperatorTree::Intersect(terms) = optimised else {
            panic!("expected scored Intersect");
        };
        assert_eq!(terms.len(), 2);
        assert!(terms
            .iter()
            .any(|term| matches!(term, OperatorTree::Union(_))));
    }

    #[test]
    fn merge_vector_thresholds_keeps_max() {
        let v1 = OperatorTree::VectorSimilarity {
            query_vector: vec![1.0, 0.0],
            threshold: 0.5,
            field: "emb".into(),
        };
        let v2 = OperatorTree::VectorSimilarity {
            query_vector: vec![1.0, 0.0],
            threshold: 0.7,
            field: "emb".into(),
        };
        let op = OperatorTree::Intersect(vec![v1, v2]);
        let optimised = QueryOptimizer::new().optimize(op);
        match optimised {
            OperatorTree::VectorSimilarity { threshold, .. } => {
                assert!((threshold - 0.7).abs() < 1e-6);
            }
            _ => panic!("expected single VectorSimilarity"),
        }
    }

    #[test]
    fn optimizer_reaches_children_inside_physical_wrappers() {
        let vector = |threshold| OperatorTree::VectorSimilarity {
            query_vector: vec![1.0, 0.0],
            threshold,
            field: "emb".into(),
        };
        let op = OperatorTree::Opaque {
            kind: "test_wrapper".into(),
            children: vec![OperatorTree::DeepFusion {
                layers: vec![DeepFusionLayer::Signal {
                    signals: vec![OperatorTree::ProgressiveFusion {
                        stages: vec![ProgressiveFusionEntry {
                            signal: OperatorTree::MessagePassing {
                                source: Box::new(OperatorTree::Intersect(vec![
                                    vector(0.5),
                                    vector(0.7),
                                ])),
                            },
                            k: 10,
                        }],
                        alpha: 0.5,
                        gating: uqa_operators::GatingSpec::Pass,
                    }],
                }],
                alpha: 0.5,
                gating: uqa_operators::GatingSpec::Pass,
            }],
            meta: std::collections::BTreeMap::new(),
        };

        let optimized = QueryOptimizer::new().optimize(op);
        let OperatorTree::Opaque { children, .. } = optimized else {
            panic!("expected opaque wrapper");
        };
        let OperatorTree::DeepFusion { layers, .. } = &children[0] else {
            panic!("expected deep-fusion wrapper");
        };
        let DeepFusionLayer::Signal { signals } = &layers[0] else {
            panic!("expected signal layer");
        };
        let OperatorTree::ProgressiveFusion { stages, .. } = &signals[0] else {
            panic!("expected progressive-fusion wrapper");
        };
        let OperatorTree::MessagePassing { source } = &stages[0].signal else {
            panic!("expected message-passing wrapper");
        };
        let OperatorTree::VectorSimilarity { threshold, .. } = source.as_ref() else {
            panic!("expected merged vector leaf");
        };
        assert!((*threshold - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn query_local_index_candidate_enables_physical_scan_rewrite() {
        let predicate = Predicate::Equals(uqa_core::Value::Int(2026));
        let optimized = QueryOptimizer::new()
            .with_row_count(1_000)
            .with_index_candidates(
                [IndexScanCandidate {
                    index_name: "docs_year_idx".into(),
                    table_name: "docs".into(),
                    field: "year".into(),
                    predicate: predicate.clone(),
                    scan_cost: 2.0,
                }],
                "docs",
            )
            .optimize(OperatorTree::Filter {
                field: "year".into(),
                predicate,
                source: None,
            });

        assert!(matches!(
            optimized,
            OperatorTree::IndexScan {
                ref index_name,
                ref field,
                ..
            } if index_name == "docs_year_idx" && field == "year"
        ));
    }
}
