//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `QueryOptimizer`: 1:1 port of `uqa/planner/optimizer.py`.
//!
//! Walks an [`OperatorTree`] and applies the ten rewrite stages from
//! Theorem 6.1.2 (Paper 1) and Theorem 6.1.1 (Paper 2):
//!
//! 1. `simplify_algebra` -- idempotent / absorption / empty
//!    elimination on Intersect / Union.
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
    EdgePatternIR, GraphPatternIR, OperatorTree, ProbBoolMode, VertexPatternIR, VertexPredicate,
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
            op = self.fuse_join_pattern(op);
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
                // Idempotent: dedup by identity (fingerprint).
                let mut seen: Vec<OperatorTree> = Vec::new();
                'outer: for child in operands {
                    let fp = child.fingerprint();
                    for s in &seen {
                        if s.fingerprint() == fp {
                            continue 'outer;
                        }
                    }
                    seen.push(child);
                }
                let operands = seen;
                // Absorption: drop Union(A, ...) when A also appears
                // in the intersection.
                let mut absorbed: Vec<OperatorTree> = Vec::new();
                for child in &operands {
                    if let OperatorTree::Union(union_operands) = child {
                        let drop = operands.iter().any(|other| {
                            other.fingerprint() != child.fingerprint()
                                && union_operands
                                    .iter()
                                    .any(|u| u.fingerprint() == other.fingerprint())
                        });
                        if drop {
                            continue;
                        }
                    }
                    absorbed.push(child.clone());
                }
                if absorbed.len() == 1 {
                    return absorbed.into_iter().next().unwrap();
                }
                OperatorTree::Intersect(absorbed)
            }
            OperatorTree::Union(operands) => {
                // Drop empty children.
                let mut kept: Vec<OperatorTree> =
                    operands.into_iter().filter(|c| !c.is_empty()).collect();
                // Idempotent: dedup by identity.
                let mut seen: Vec<OperatorTree> = Vec::new();
                'outer: for child in kept.drain(..) {
                    let fp = child.fingerprint();
                    for s in &seen {
                        if s.fingerprint() == fp {
                            continue 'outer;
                        }
                    }
                    seen.push(child);
                }
                let operands = seen;
                // Absorption: drop Intersect(A, ...) when A also appears
                // in the union.
                let mut absorbed: Vec<OperatorTree> = Vec::new();
                for child in &operands {
                    if let OperatorTree::Intersect(int_operands) = child {
                        let drop = operands.iter().any(|other| {
                            other.fingerprint() != child.fingerprint()
                                && int_operands
                                    .iter()
                                    .any(|u| u.fingerprint() == other.fingerprint())
                        });
                        if drop {
                            continue;
                        }
                    }
                    absorbed.push(child.clone());
                }
                if absorbed.len() == 1 {
                    return absorbed.into_iter().next().unwrap();
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
        match op {
            OperatorTree::Intersect(ops) => {
                OperatorTree::Intersect(ops.into_iter().map(|o| self.simplify_algebra(o)).collect())
            }
            OperatorTree::Union(ops) => {
                OperatorTree::Union(ops.into_iter().map(|o| self.simplify_algebra(o)).collect())
            }
            OperatorTree::Complement(inner) => {
                OperatorTree::Complement(Box::new(self.simplify_algebra(*inner)))
            }
            OperatorTree::Filter {
                field,
                predicate,
                source: Some(s),
            } => OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.simplify_algebra(*s))),
            },
            OperatorTree::Composed(ops) => {
                OperatorTree::Composed(ops.into_iter().map(|o| self.simplify_algebra(o)).collect())
            }
            other => other,
        }
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
                    // Try edge variable: "src_tgt.prop". Mirror Python's
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
        match op {
            OperatorTree::Intersect(ops) => OperatorTree::Intersect(
                ops.into_iter()
                    .map(|o| self.push_graph_pattern_filters(o))
                    .collect(),
            ),
            OperatorTree::Union(ops) => OperatorTree::Union(
                ops.into_iter()
                    .map(|o| self.push_graph_pattern_filters(o))
                    .collect(),
            ),
            OperatorTree::Complement(inner) => {
                OperatorTree::Complement(Box::new(self.push_graph_pattern_filters(*inner)))
            }
            OperatorTree::Filter {
                field,
                predicate,
                source: Some(s),
            } => OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.push_graph_pattern_filters(*s))),
            },
            OperatorTree::Composed(ops) => OperatorTree::Composed(
                ops.into_iter()
                    .map(|o| self.push_graph_pattern_filters(o))
                    .collect(),
            ),
            other => other,
        }
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
        match op {
            OperatorTree::Intersect(ops) => OperatorTree::Intersect(
                ops.into_iter()
                    .map(|o| self.push_filter_into_traverse(o))
                    .collect(),
            ),
            OperatorTree::Union(ops) => OperatorTree::Union(
                ops.into_iter()
                    .map(|o| self.push_filter_into_traverse(o))
                    .collect(),
            ),
            OperatorTree::Complement(inner) => {
                OperatorTree::Complement(Box::new(self.push_filter_into_traverse(*inner)))
            }
            OperatorTree::Filter {
                field,
                predicate,
                source: Some(s),
            } => OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.push_filter_into_traverse(*s))),
            },
            OperatorTree::Composed(ops) => OperatorTree::Composed(
                ops.into_iter()
                    .map(|o| self.push_filter_into_traverse(o))
                    .collect(),
            ),
            other => other,
        }
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
        match op {
            OperatorTree::Intersect(ops) => OperatorTree::Intersect(
                ops.into_iter()
                    .map(|o| self.push_filter_below_graph_join(o))
                    .collect(),
            ),
            OperatorTree::Union(ops) => OperatorTree::Union(
                ops.into_iter()
                    .map(|o| self.push_filter_below_graph_join(o))
                    .collect(),
            ),
            OperatorTree::Complement(inner) => {
                OperatorTree::Complement(Box::new(self.push_filter_below_graph_join(*inner)))
            }
            OperatorTree::Filter {
                field,
                predicate,
                source: Some(s),
            } => OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.push_filter_below_graph_join(*s))),
            },
            OperatorTree::Composed(ops) => OperatorTree::Composed(
                ops.into_iter()
                    .map(|o| self.push_filter_below_graph_join(o))
                    .collect(),
            ),
            other => other,
        }
    }

    // ---------------------------------------------------------------
    // 6. Fuse PatternMatch operators inside an Intersect
    // ---------------------------------------------------------------

    fn fuse_join_pattern(&self, op: OperatorTree) -> OperatorTree {
        if let OperatorTree::Intersect(ops) = op {
            let children: Vec<OperatorTree> =
                ops.into_iter().map(|o| self.fuse_join_pattern(o)).collect();
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
                    let last = merged.last().unwrap().clone();
                    match Self::merge_patterns(&last, &pm) {
                        Some(fused) => *merged.last_mut().unwrap() = fused,
                        None => merged.push(pm),
                    }
                }
                pattern_ops = merged;
            }
            let mut all = other_ops;
            all.extend(pattern_ops);
            if all.len() == 1 {
                return all.into_iter().next().unwrap();
            }
            return OperatorTree::Intersect(all);
        }
        match op {
            OperatorTree::Union(ops) => {
                OperatorTree::Union(ops.into_iter().map(|o| self.fuse_join_pattern(o)).collect())
            }
            OperatorTree::Complement(inner) => {
                OperatorTree::Complement(Box::new(self.fuse_join_pattern(*inner)))
            }
            OperatorTree::Filter {
                field,
                predicate,
                source: Some(s),
            } => OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.fuse_join_pattern(*s))),
            },
            OperatorTree::Composed(ops) => {
                OperatorTree::Composed(ops.into_iter().map(|o| self.fuse_join_pattern(o)).collect())
            }
            other => other,
        }
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
        // Python uses a regular dict for the merge buffer, which keeps
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
                let (mut q, mut t, mut f) = (
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
                let _ = (&mut q, &mut f);
                merged_vectors.push(OperatorTree::VectorSimilarity {
                    query_vector: q,
                    threshold: t,
                    field: f,
                });
            }
            let mut all = other_ops;
            all.extend(merged_vectors);
            if all.len() == 1 {
                return all.into_iter().next().unwrap();
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
            // Mirror Python: the optimizer ranks intersect arms by the
            // algebraic operator cost (`CostModel.estimate`), not the
            // cardinality estimator. The two diverge for `Filter`,
            // `Score`, `Traverse`, `RegularPathQuery`, fusion / hybrid
            // / cross-paradigm join nodes, and any operator with a
            // dedicated formula in `cost_model.py`.
            let cost_stats = uqa_core::IndexStats::new(self.row_count.unwrap_or(1_000));
            children.sort_by(|a, b| {
                let ca = self.cost_model.estimate(a, &cost_stats);
                let cb = self.cost_model.estimate(b, &cost_stats);
                ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
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
            OperatorTree::LogOddsFusion {
                signals,
                alpha,
                gating,
            } => {
                let mut sigs: Vec<OperatorTree> = signals
                    .into_iter()
                    .map(|s| self.reorder_fusion_signals(s))
                    .collect();
                sigs.sort_by(|a, b| {
                    let ca = self.graph_aware_signal_cost(a);
                    let cb = self.graph_aware_signal_cost(b);
                    ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
                });
                OperatorTree::LogOddsFusion {
                    signals: sigs,
                    alpha,
                    gating,
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
                    ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
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
        match op {
            OperatorTree::Intersect(ops) => OperatorTree::Intersect(
                ops.into_iter()
                    .map(|o| self.reorder_fusion_signals(o))
                    .collect(),
            ),
            OperatorTree::Union(ops) => OperatorTree::Union(
                ops.into_iter()
                    .map(|o| self.reorder_fusion_signals(o))
                    .collect(),
            ),
            OperatorTree::Complement(inner) => {
                OperatorTree::Complement(Box::new(self.reorder_fusion_signals(*inner)))
            }
            OperatorTree::Filter {
                field,
                predicate,
                source: Some(s),
            } => OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.reorder_fusion_signals(*s))),
            },
            OperatorTree::Composed(ops) => OperatorTree::Composed(
                ops.into_iter()
                    .map(|o| self.reorder_fusion_signals(o))
                    .collect(),
            ),
            other => other,
        }
    }

    // ---------------------------------------------------------------
    // 10. Substitute leaf Filter with IndexScan
    // ---------------------------------------------------------------

    fn apply_index_scan(&self, op: OperatorTree) -> OperatorTree {
        let (Some(im), Some(table)) = (&self.index_manager, &self.table_name) else {
            return op;
        };
        if let OperatorTree::Filter {
            field,
            predicate,
            source: None,
        } = &op
        {
            if let Some((name, scan_cost)) =
                im.find_covering_index_with_cost(table, field, predicate)
            {
                // Python's `_apply_index_scan` only rewrites when the
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
        match op {
            OperatorTree::Intersect(ops) => {
                OperatorTree::Intersect(ops.into_iter().map(|o| self.apply_index_scan(o)).collect())
            }
            OperatorTree::Union(ops) => {
                OperatorTree::Union(ops.into_iter().map(|o| self.apply_index_scan(o)).collect())
            }
            OperatorTree::Complement(inner) => {
                OperatorTree::Complement(Box::new(self.apply_index_scan(*inner)))
            }
            OperatorTree::Composed(ops) => {
                OperatorTree::Composed(ops.into_iter().map(|o| self.apply_index_scan(o)).collect())
            }
            other => other,
        }
    }

    // ---------------------------------------------------------------
    // Generic recursion (used by simplify / merge_vector / reorder)
    // ---------------------------------------------------------------

    fn recurse_children(&self, op: OperatorTree) -> OperatorTree {
        match op {
            OperatorTree::Intersect(ops) => {
                OperatorTree::Intersect(ops.into_iter().map(|o| self.optimize(o)).collect())
            }
            OperatorTree::Union(ops) => {
                OperatorTree::Union(ops.into_iter().map(|o| self.optimize(o)).collect())
            }
            OperatorTree::Complement(inner) => {
                OperatorTree::Complement(Box::new(self.optimize(*inner)))
            }
            OperatorTree::Filter {
                field,
                predicate,
                source: Some(s),
            } => OperatorTree::Filter {
                field,
                predicate,
                source: Some(Box::new(self.optimize(*s))),
            },
            OperatorTree::Composed(ops) => {
                OperatorTree::Composed(ops.into_iter().map(|o| self.optimize(o)).collect())
            }
            OperatorTree::Score {
                scorer,
                source,
                query_terms,
                field,
            } => OperatorTree::Score {
                scorer,
                source: Box::new(self.optimize(*source)),
                query_terms,
                field,
            },
            OperatorTree::LogOddsFusion {
                signals,
                alpha,
                gating,
            } => OperatorTree::LogOddsFusion {
                signals: signals.into_iter().map(|s| self.optimize(s)).collect(),
                alpha,
                gating,
            },
            OperatorTree::ProbBoolFusion { signals, mode } => OperatorTree::ProbBoolFusion {
                signals: signals.into_iter().map(|s| self.optimize(s)).collect(),
                mode,
            },
            OperatorTree::ProbNot {
                signal,
                default_prob,
            } => OperatorTree::ProbNot {
                signal: Box::new(self.optimize(*signal)),
                default_prob,
            },
            OperatorTree::AttentionFusion {
                signals,
                attention,
                query_features,
            } => OperatorTree::AttentionFusion {
                signals: signals.into_iter().map(|s| self.optimize(s)).collect(),
                attention,
                query_features,
            },
            OperatorTree::LearnedFusion { signals, learned } => OperatorTree::LearnedFusion {
                signals: signals.into_iter().map(|s| self.optimize(s)).collect(),
                learned,
            },
            OperatorTree::SparseThreshold { source, threshold } => OperatorTree::SparseThreshold {
                source: Box::new(self.optimize(*source)),
                threshold,
            },
            OperatorTree::CosineProbability(inner) => {
                OperatorTree::CosineProbability(Box::new(self.optimize(*inner)))
            }
            OperatorTree::GraphJoin {
                left,
                right,
                label,
                graph,
            } => OperatorTree::GraphJoin {
                left: Box::new(self.optimize(*left)),
                right: Box::new(self.optimize(*right)),
                label,
                graph,
            },
            OperatorTree::Aggregate {
                source: Some(s),
                field,
                monoid,
            } => OperatorTree::Aggregate {
                source: Some(Box::new(self.optimize(*s))),
                field,
                monoid,
            },
            OperatorTree::GroupBy {
                source,
                group_field,
                agg_field,
                monoid,
            } => OperatorTree::GroupBy {
                source: Box::new(self.optimize(*source)),
                group_field,
                agg_field,
                monoid,
            },
            other => other,
        }
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
        }
    }

    #[test]
    fn empty_intersect_collapses_to_intersect_empty() {
        let op = OperatorTree::Intersect(vec![term("a"), OperatorTree::Intersect(vec![])]);
        let optimised = QueryOptimizer::new().optimize(op);
        assert!(optimised.is_empty());
    }

    #[test]
    fn distinct_clones_in_intersect_keep_both() {
        // Mirrors Python's `is` semantics: two cloned operators have
        // different identities so the optimizer leaves them intact.
        let op = OperatorTree::Intersect(vec![term("a"), term("a")]);
        let optimised = QueryOptimizer::new().optimize(op);
        // After cardinality reorder both copies survive; the result
        // remains an Intersect with two children.
        if let OperatorTree::Intersect(ops) = optimised {
            assert_eq!(ops.len(), 2);
        } else {
            panic!("expected Intersect");
        }
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
}
