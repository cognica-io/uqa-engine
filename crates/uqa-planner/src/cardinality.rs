//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cardinality estimation -- 1:1 port of `uqa.planner.cardinality`.
//!
//! Two estimator surfaces share a single [`CardinalityEstimator`]:
//!
//! * **AST-Expr surface**: `selectivity(predicate: &Expr, stats:
//!   &RelationStats)` and `estimate_rows(predicate, stats)` work on
//!   the SQL AST. Used by the relational planner / DPccp join
//!   enumerator.
//!
//! * **Operator-tree surface**: `estimate(op: &OperatorTree, stats:
//!   &IndexStats)` mirrors Python's
//!   `CardinalityEstimator.estimate(op, stats)`. Walks the
//!   [`OperatorTree`] variants and applies the selectivity tables,
//!   damping exponents, entropy lower bounds, graph statistics, and
//!   random-walk sampling fallback from Definition 6.2.3 (Paper 1)
//!   and Theorem 6.3.2 (Paper 2).
//!
//! The estimator surfaces are independent: callers pick whichever
//! matches their plan IR. Both share `column_stats` /
//! `default_selectivity` / `like_selectivity` / `range_selectivity`
//! configuration.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{Predicate, Value};
use uqa_operators::{
    DeepFusionLayer, GraphPatternIR, MultiStageCutoff, OperatorTree, ProbBoolMode,
    TemporalFilterIR, VertexConstraint,
};
use uqa_sql::ast::{BinaryOp, Expr};

/// Jaccard-style selectivity assumed for text-similarity joins when no
/// per-column statistics are available. Mirrors
/// `JACCARD_JOIN_SELECTIVITY` in the Python reference.
pub const JACCARD_JOIN_SELECTIVITY: f64 = 0.05;

/// Default vector-similarity join selectivity. Matches the Python
/// `VECTOR_JOIN_SELECTIVITY` constant.
pub const VECTOR_JOIN_SELECTIVITY: f64 = 0.1;

/// Fallback average out-degree used by graph traversal cardinality
/// when no [`GraphStats`] is supplied.
pub const GRAPH_AVG_DEGREE_DEFAULT: f64 = 10.0;

// ---------------------------------------------------------------------
// Graph statistics
// ---------------------------------------------------------------------

/// Graph-level statistics for cardinality estimation
/// (Theorem 6.3.2, Paper 2). Mirrors
/// `uqa.planner.cardinality.GraphStats`.
#[derive(Debug, Clone, Default)]
pub struct GraphStats {
    pub num_vertices: u64,
    pub num_edges: u64,
    pub label_counts: BTreeMap<String, u64>,
    pub avg_out_degree: f64,
    pub degree_distribution: BTreeMap<u64, u64>,
    pub min_timestamp: Option<f64>,
    pub max_timestamp: Option<f64>,
    pub graph_name: String,
    pub vertex_label_counts: BTreeMap<String, u64>,
    pub label_degree_map: BTreeMap<String, f64>,
}

impl GraphStats {
    /// Fraction of edges matching `label`. `None` is the wildcard label
    /// (full edge population).
    pub fn label_selectivity(&self, label: Option<&str>) -> f64 {
        match label {
            None => 1.0,
            Some(_) if self.num_edges == 0 => 1.0,
            Some(name) => {
                let c = self.label_counts.get(name).copied().unwrap_or(0);
                c as f64 / self.num_edges as f64
            }
        }
    }

    /// Edge density `|E| / |V|^2`.
    pub fn edge_density(&self) -> f64 {
        if self.num_vertices <= 1 {
            return 0.0;
        }
        let nv = self.num_vertices as f64;
        self.num_edges as f64 / (nv * nv)
    }
}

// ---------------------------------------------------------------------
// Per-(field, term) IndexStats. Mirrors `uqa.core.types.IndexStats`.
// ---------------------------------------------------------------------

/// Document-frequency statistics for an inverted index. The Python
/// reference exposes `total_docs` plus `doc_freq(field, term)`; the
/// Rust port stores frequencies as a `BTreeMap<(field, term), u64>`
/// for deterministic iteration.
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub total_docs: u64,
    pub doc_freqs: BTreeMap<(String, String), u64>,
}

impl IndexStats {
    pub fn new(total_docs: u64) -> Self {
        Self {
            total_docs,
            doc_freqs: BTreeMap::new(),
        }
    }

    /// Document frequency of `term` in `field`. Returns 0 when the
    /// (field, term) pair has no recorded frequency.
    pub fn doc_freq(&self, field: &str, term: &str) -> u64 {
        self.doc_freqs
            .get(&(field.to_string(), term.to_string()))
            .copied()
            .unwrap_or(0)
    }

    /// Set the frequency of `term` in `field`.
    pub fn with_doc_freq(
        mut self,
        field: impl Into<String>,
        term: impl Into<String>,
        n: u64,
    ) -> Self {
        self.doc_freqs.insert((field.into(), term.into()), n);
        self
    }
}

// ---------------------------------------------------------------------
// Random-walk sampler trait used by `_sample_graph_cardinality`.
// ---------------------------------------------------------------------

/// One outgoing edge surfaced by a [`GraphStoreSampler`].
pub struct EdgeSample {
    pub target_id: u64,
    pub label: String,
}

/// Minimal graph store interface used by the sampler. Mirrors the
/// `_vertices` / `_adj_out` / `_edges` private fields the Python
/// reference reads.
pub trait GraphStoreSampler: Send + Sync {
    /// IDs of every vertex in the store.
    fn vertex_ids(&self) -> Vec<u64>;

    /// Outgoing edges from `vid`.
    fn outgoing_edges(&self, vid: u64) -> Vec<EdgeSample>;

    /// Apply a vertex constraint closure. The Python reference calls
    /// `c(vertex)` directly; the Rust port packages this as a callback
    /// so the sampler can hide vertex storage behind whichever store
    /// implements the trait.
    fn vertex_satisfies(&self, vid: u64, constraint: &VertexConstraint) -> bool;
}

// ---------------------------------------------------------------------
// Per-column statistics (used by both AST-Expr and operator surfaces).
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ColumnStats {
    pub distinct_count: u64,
    pub null_count: u64,
    pub min_value: Option<Value>,
    pub max_value: Option<Value>,
    pub row_count: u64,
    /// Equi-depth histogram bucket boundaries, sorted ascending.
    /// `b+1` boundaries describe `b` buckets.
    pub histogram: Vec<Value>,
    /// Most-common values, descending by frequency.
    pub mcv_values: Vec<Value>,
    pub mcv_frequencies: Vec<f64>,
}

impl ColumnStats {
    /// Default selectivity of an equality predicate over this column.
    pub fn equality_selectivity(&self) -> f64 {
        if self.distinct_count == 0 {
            1.0
        } else {
            1.0 / self.distinct_count as f64
        }
    }

    pub fn matches_mcv(&self, value: &Value) -> Option<f64> {
        for (mcv, freq) in self.mcv_values.iter().zip(self.mcv_frequencies.iter()) {
            if mcv == value {
                return Some(*freq);
            }
        }
        None
    }
}

#[derive(Debug, Clone, Default)]
pub struct RelationStats {
    pub row_count: u64,
    pub columns: BTreeMap<String, ColumnStats>,
}

impl RelationStats {
    pub fn new(row_count: u64) -> Self {
        Self {
            row_count,
            columns: BTreeMap::new(),
        }
    }

    pub fn with_column(mut self, name: impl Into<String>, stats: ColumnStats) -> Self {
        self.columns.insert(name.into(), stats);
        self
    }

    pub fn column(&self, name: &str) -> Option<&ColumnStats> {
        self.columns.get(name)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Selectivity(pub f64);

impl Selectivity {
    pub fn clamp(self) -> Self {
        Self(self.0.clamp(0.0, 1.0))
    }

    pub fn raw(self) -> f64 {
        self.0
    }
}

// ---------------------------------------------------------------------
// CardinalityEstimator
// ---------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct CardinalityEstimator {
    /// Default selectivity for an unknown predicate.
    pub default_selectivity: f64,
    /// Default selectivity for a `LIKE 'foo%'` style prefix match.
    pub like_selectivity: f64,
    /// Default selectivity for an inequality predicate with no histogram.
    pub range_selectivity: f64,
    /// Per-column statistics keyed by column name. Used by the
    /// operator-tree filter selectivity helpers.
    pub column_stats: BTreeMap<String, ColumnStats>,
    /// Optional graph statistics; enables Theorem 6.3.2 estimation.
    pub graph_stats: Option<GraphStats>,
    /// Optional graph store sampler; enables random-walk sampling for
    /// pattern matches on large graphs.
    pub graph_store: Option<Arc<dyn GraphStoreSampler>>,
}

impl CardinalityEstimator {
    pub fn new() -> Self {
        Self {
            default_selectivity: 0.1,
            like_selectivity: 0.05,
            range_selectivity: 0.3,
            column_stats: BTreeMap::new(),
            graph_stats: None,
            graph_store: None,
        }
    }

    pub fn with_column_stats(mut self, stats: BTreeMap<String, ColumnStats>) -> Self {
        self.column_stats = stats;
        self
    }

    pub fn with_graph_stats(mut self, stats: GraphStats) -> Self {
        self.graph_stats = Some(stats);
        self
    }

    pub fn with_graph_store(mut self, store: Arc<dyn GraphStoreSampler>) -> Self {
        self.graph_store = Some(store);
        self
    }

    // -----------------------------------------------------------------
    // AST-Expr surface (relational planner / DPccp).
    // -----------------------------------------------------------------

    /// Estimate the selectivity of `predicate` against `stats`. Best
    /// effort: unknown shapes fall back on
    /// [`Self::default_selectivity`].
    pub fn selectivity(&self, predicate: &Expr, stats: &RelationStats) -> Selectivity {
        match predicate {
            Expr::And(parts) => {
                let mut s = 1.0;
                for p in parts {
                    s *= self.selectivity(p, stats).raw();
                }
                Selectivity(s).clamp()
            }
            Expr::Or(parts) => {
                let mut anti = 1.0;
                for p in parts {
                    anti *= 1.0 - self.selectivity(p, stats).raw();
                }
                Selectivity(1.0 - anti).clamp()
            }
            Expr::Not(inner) => Selectivity(1.0 - self.selectivity(inner, stats).raw()).clamp(),
            Expr::IsNull { expr, negated } => {
                let col = column_of(expr);
                let null_frac = col
                    .and_then(|c| stats.column(c))
                    .map(|s| {
                        if stats.row_count == 0 {
                            0.0
                        } else {
                            s.null_count as f64 / stats.row_count as f64
                        }
                    })
                    .unwrap_or(0.05);
                let s = if *negated { 1.0 - null_frac } else { null_frac };
                Selectivity(s).clamp()
            }
            Expr::Binary { op, lhs, rhs } => self.binary_selectivity(*op, lhs, rhs, stats),
            Expr::InList { list, negated, .. } => {
                let s = (list.len() as f64) * self.default_selectivity;
                let s = s.min(1.0);
                Selectivity(if *negated { 1.0 - s } else { s }).clamp()
            }
            Expr::Between { .. } => Selectivity(self.range_selectivity).clamp(),
            _ => Selectivity(self.default_selectivity).clamp(),
        }
    }

    fn binary_selectivity(
        &self,
        op: BinaryOp,
        lhs: &Expr,
        rhs: &Expr,
        stats: &RelationStats,
    ) -> Selectivity {
        let col = column_of(lhs).or_else(|| column_of(rhs));
        let constant = literal_of(rhs).or_else(|| literal_of(lhs));
        let col_stats = col.and_then(|name| stats.column(name));
        let value = constant;
        match op {
            BinaryOp::Equal => {
                if let (Some(stats), Some(v)) = (col_stats, value) {
                    if let Some(freq) = stats.matches_mcv(v) {
                        return Selectivity(freq).clamp();
                    }
                    return Selectivity(stats.equality_selectivity()).clamp();
                }
                Selectivity(self.default_selectivity).clamp()
            }
            BinaryOp::NotEqual => {
                let eq = self
                    .binary_selectivity(BinaryOp::Equal, lhs, rhs, stats)
                    .raw();
                Selectivity(1.0 - eq).clamp()
            }
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                if let (Some(stats), Some(v)) = (col_stats, value) {
                    if let Some(s) = histogram_range_selectivity(stats, op, v) {
                        return Selectivity(s).clamp();
                    }
                }
                Selectivity(self.range_selectivity).clamp()
            }
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
                Selectivity(self.default_selectivity).clamp()
            }
        }
    }

    /// Estimated row count after applying `predicate` to `stats`.
    pub fn estimate_rows(&self, predicate: &Expr, stats: &RelationStats) -> u64 {
        let s = self.selectivity(predicate, stats).raw();
        ((stats.row_count as f64) * s).round() as u64
    }

    // -----------------------------------------------------------------
    // Operator-tree surface (1:1 with Python `CardinalityEstimator`).
    // -----------------------------------------------------------------

    /// Estimate the cardinality of `op` against an inverted index
    /// described by `stats`. Mirrors `CardinalityEstimator.estimate(op,
    /// stats)`.
    pub fn estimate(&self, op: &OperatorTree, stats: &IndexStats) -> f64 {
        let n = if stats.total_docs > 0 {
            stats.total_docs as f64
        } else {
            1.0
        };

        match op {
            OperatorTree::Empty => 0.0,
            OperatorTree::Term { query, field } => {
                let field_name = field.as_deref().unwrap_or("_default");
                stats.doc_freq(field_name, query) as f64
            }
            OperatorTree::VectorSimilarity { threshold, .. } => {
                n * Self::vector_selectivity(*threshold)
            }
            OperatorTree::KNN { k, .. } => *k as f64,
            OperatorTree::Filter {
                field, predicate, ..
            } => n * self.filter_selectivity(field, predicate, n),
            OperatorTree::Score { source, .. } => self.estimate(source, stats),
            OperatorTree::Intersect(ops) => self.estimate_intersect(ops, stats, n),
            OperatorTree::Union(ops) => {
                let child_cards: f64 = ops.iter().map(|o| self.estimate(o, stats)).sum();
                n.min(child_cards)
            }
            OperatorTree::Complement(inner) => {
                let inner_card = self.estimate(inner, stats);
                (n - inner_card).max(0.0)
            }
            _ => self.estimate_cross_paradigm(op, stats, n),
        }
    }

    /// Backward-compat alias used by [`super::query_optimizer`]. Builds
    /// an [`IndexStats`] with `total_docs = row_count.unwrap_or(1000)`
    /// and routes through [`Self::estimate`].
    pub fn estimate_operator(&self, op: &OperatorTree, row_count: Option<u64>) -> f64 {
        let stats = IndexStats::new(row_count.unwrap_or(1_000));
        self.estimate(op, &stats)
    }

    fn estimate_intersect(&self, ops: &[OperatorTree], stats: &IndexStats, n: f64) -> f64 {
        let mut child_cards: Vec<f64> = ops.iter().map(|o| self.estimate(o, stats)).collect();
        if child_cards.is_empty() {
            return 0.0;
        }
        child_cards.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let damping = self.intersection_damping(ops);
        let mut result = child_cards[0];
        for card in &child_cards[1..] {
            let sel = if n > 0.0 { card / n } else { 1.0 };
            result *= sel.powf(damping);
        }

        // Apply entropy-based lower bound (Paper 1, Section 7) when
        // column stats are available.
        if !self.column_stats.is_empty() {
            let mut entropies: Vec<f64> = Vec::new();
            for op_item in ops {
                if let OperatorTree::Filter { field, .. } = op_item {
                    if let Some(cs) = self.column_stats.get(field) {
                        entropies.push(column_entropy(cs));
                    }
                }
            }
            if !entropies.is_empty() {
                let lb = entropy_cardinality_lower_bound(n, &entropies);
                result = result.max(lb);
            }
        }

        result.max(1.0)
    }

    /// Choose damping exponent based on predicate correlation. Mirrors
    /// `_intersection_damping`.
    fn intersection_damping(&self, ops: &[OperatorTree]) -> f64 {
        let fields: Vec<&str> = ops
            .iter()
            .filter_map(|o| match o {
                OperatorTree::Filter { field, .. } => Some(field.as_str()),
                _ => None,
            })
            .collect();

        if fields.len() < 2 {
            return 0.5;
        }

        let unique: std::collections::BTreeSet<&str> = fields.iter().copied().collect();
        if unique.len() == 1 {
            return 0.1;
        }

        if !self.column_stats.is_empty() && fields.len() >= 2 {
            let cs_a = self.column_stats.get(fields[0]);
            let cs_b = self.column_stats.get(fields[1]);
            if let (Some(a), Some(b)) = (cs_a, cs_b) {
                let mi = mutual_information_estimate(a, b, 0.1);
                if mi > 1.0 {
                    return 0.2;
                }
                if mi > 0.5 {
                    return 0.3;
                }
            }
        }

        0.5
    }

    /// Cardinality estimation for cross-paradigm operators. Mirrors
    /// `_estimate_cross_paradigm`.
    fn estimate_cross_paradigm(&self, op: &OperatorTree, stats: &IndexStats, n: f64) -> f64 {
        match op {
            OperatorTree::MultiStage { stages } => {
                if let Some(last) = stages.last() {
                    match last.cutoff {
                        MultiStageCutoff::TopK(k) => k as f64,
                        MultiStageCutoff::Ratio(r) => n * r,
                    }
                } else {
                    n * 0.5
                }
            }

            OperatorTree::AttentionFusion { signals, .. }
            | OperatorTree::LearnedFusion { signals, .. }
            | OperatorTree::LogOddsFusion { signals, .. } => {
                let sum: f64 = signals.iter().map(|s| self.estimate(s, stats)).sum();
                n.min(sum)
            }

            OperatorTree::MultiFieldSearch { fields, .. } => n.min(n * 0.3 * fields.len() as f64),

            OperatorTree::SparseThreshold { source, .. } => self.estimate(source, stats) * 0.5,

            OperatorTree::VectorExclusion { positive, negative } => {
                let pos = self.estimate(positive, stats);
                let neg = self.estimate(negative, stats);
                let overlap = if n > 0.0 { (pos * neg) / n } else { 0.0 };
                (pos - overlap).max(1.0)
            }

            OperatorTree::FacetVector { vector_op, .. } => self.estimate(vector_op, stats),

            OperatorTree::VertexAggregation { .. } => 1.0,

            OperatorTree::ProbBoolFusion { signals, mode } => {
                let cards: Vec<f64> = signals.iter().map(|s| self.estimate(s, stats)).collect();
                match mode {
                    ProbBoolMode::And => {
                        if cards.is_empty() {
                            0.0
                        } else {
                            let mut result = cards[0];
                            for c in &cards[1..] {
                                if n > 0.0 {
                                    result = (result * c) / n;
                                }
                            }
                            result.max(1.0)
                        }
                    }
                    ProbBoolMode::Or => n.min(cards.iter().sum()),
                }
            }

            OperatorTree::ProbNot { signal, .. } => {
                let inner = self.estimate(signal, stats);
                (n - inner).max(0.0)
            }

            OperatorTree::HybridTextVector {
                term_op, vector_op, ..
            } => {
                let text = self.estimate(term_op, stats);
                let vec_card = self.estimate(vector_op, stats);
                if n > 0.0 {
                    ((text * vec_card) / n).max(1.0)
                } else {
                    1.0
                }
            }

            OperatorTree::SemanticFilter { source, vector_op } => {
                let src = self.estimate(source, stats);
                let vec_card = self.estimate(vector_op, stats);
                if n > 0.0 {
                    ((src * vec_card) / n).max(1.0)
                } else {
                    1.0
                }
            }

            OperatorTree::TemporalTraverse {
                label,
                max_hops,
                temporal_filter,
                ..
            } => self.estimate_traverse(label.as_deref(), *max_hops, n, temporal_filter.as_ref()),

            OperatorTree::TemporalPatternMatch {
                pattern,
                temporal_filter,
                ..
            } => self.estimate_temporal_pattern_match(pattern, temporal_filter.as_ref(), n),

            OperatorTree::Traverse {
                label, max_hops, ..
            } => self.estimate_traverse(label.as_deref(), *max_hops, n, None),

            OperatorTree::PatternMatch { pattern, .. } => self.estimate_pattern_match(pattern, n),

            OperatorTree::RegularPathQuery { rpq_source, .. } => self.estimate_rpq(rpq_source, n),

            OperatorTree::WeightedPathQuery {
                rpq_source,
                predicate_selectivity,
                ..
            } => self.estimate_rpq(rpq_source, n) * predicate_selectivity,

            OperatorTree::MessagePassing { .. } | OperatorTree::GraphEmbedding { .. } => n,

            OperatorTree::PageRank { .. }
            | OperatorTree::HITS { .. }
            | OperatorTree::BetweennessCentrality { .. } => self
                .graph_stats
                .as_ref()
                .map(|gs| gs.num_vertices as f64)
                .unwrap_or(n),

            OperatorTree::TextSimilarityJoin { left, right, .. } => {
                let l = self.estimate_join_side(left, stats, n);
                let r = self.estimate_join_side(right, stats, n);
                l * r * JACCARD_JOIN_SELECTIVITY
            }

            OperatorTree::VectorSimilarityJoin { left, right, .. } => {
                let l = self.estimate_join_side(left, stats, n);
                let r = self.estimate_join_side(right, stats, n);
                l * r * VECTOR_JOIN_SELECTIVITY
            }

            OperatorTree::GraphJoin { left, label, .. } => {
                let l = self.estimate_join_side(left, stats, n);
                let avg_degree = self
                    .graph_stats
                    .as_ref()
                    .map(|gs| gs.avg_out_degree)
                    .unwrap_or(GRAPH_AVG_DEGREE_DEFAULT);
                let label_sel = self
                    .graph_stats
                    .as_ref()
                    .map(|gs| gs.label_selectivity(label.as_deref()))
                    .unwrap_or(1.0);
                l * avg_degree * label_sel
            }

            OperatorTree::HybridJoin { left, right } => {
                let l = self.estimate_join_side(left, stats, n);
                let r = self.estimate_join_side(right, stats, n);
                if n > 0.0 {
                    (l * r) / n
                } else {
                    0.0
                }
            }

            OperatorTree::CrossParadigmJoin { left, .. } => {
                let l = self.estimate_join_side(left, stats, n);
                let avg_degree = self
                    .graph_stats
                    .as_ref()
                    .map(|gs| gs.avg_out_degree)
                    .unwrap_or(GRAPH_AVG_DEGREE_DEFAULT);
                let label_sel = 1.0;
                l * avg_degree * label_sel
            }

            OperatorTree::ProgressiveFusion { stages } => {
                stages.last().map(|s| s.k as f64).unwrap_or(n)
            }

            OperatorTree::DeepFusion { layers, .. } => self.estimate_deep_fusion(layers, stats, n),

            OperatorTree::CosineProbability(inner) => self.estimate(inner, stats),

            OperatorTree::Composed(ops) => {
                ops.last().map(|o| self.estimate(o, stats)).unwrap_or(0.0)
            }

            OperatorTree::Facet { .. } => n,
            OperatorTree::IndexScan {
                field, predicate, ..
            } => n * self.filter_selectivity(field, predicate, n),
            OperatorTree::Aggregate { .. } => 1.0,
            OperatorTree::GroupBy { .. } => n * 0.1,

            OperatorTree::Opaque { children, .. } => children
                .iter()
                .map(|c| self.estimate(c, stats))
                .fold(0.0, f64::max),

            // Variants already handled in `estimate`.
            OperatorTree::Empty
            | OperatorTree::Term { .. }
            | OperatorTree::Filter { .. }
            | OperatorTree::Score { .. }
            | OperatorTree::Intersect(_)
            | OperatorTree::Union(_)
            | OperatorTree::Complement(_)
            | OperatorTree::VectorSimilarity { .. }
            | OperatorTree::KNN { .. } => n,
        }
    }

    fn estimate_deep_fusion(&self, layers: &[DeepFusionLayer], stats: &IndexStats, n: f64) -> f64 {
        let mut card: f64 = 0.0;
        for layer in layers {
            match layer {
                DeepFusionLayer::Signal { signals } => {
                    let sum: f64 = signals.iter().map(|s| self.estimate(s, stats)).sum();
                    card = card.max(n.min(sum));
                }
                DeepFusionLayer::Propagate { edge_label } => {
                    let avg_degree = self
                        .graph_stats
                        .as_ref()
                        .map(|gs| gs.avg_out_degree)
                        .unwrap_or(GRAPH_AVG_DEGREE_DEFAULT);
                    let label_sel = self
                        .graph_stats
                        .as_ref()
                        .map(|gs| gs.label_selectivity(edge_label.as_deref()))
                        .unwrap_or(1.0);
                    card = n.min(card * avg_degree * label_sel);
                }
                DeepFusionLayer::Conv => {}
                DeepFusionLayer::Pool { pool_size } => {
                    let denom = if *pool_size <= 0.0 { 1.0 } else { *pool_size };
                    card = (card / denom).max(1.0);
                }
                DeepFusionLayer::Flatten => {
                    card = 1.0;
                }
                DeepFusionLayer::Dense
                | DeepFusionLayer::Softmax
                | DeepFusionLayer::BatchNorm
                | DeepFusionLayer::Dropout => {}
            }
        }
        card.max(1.0)
    }

    /// Traverse cardinality using graph statistics (Theorem 6.3.2,
    /// Paper 2). Mirrors `_estimate_traverse`.
    fn estimate_traverse(
        &self,
        label: Option<&str>,
        hops: usize,
        n: f64,
        temporal_filter: Option<&TemporalFilterIR>,
    ) -> f64 {
        let branching = if let Some(gs) = self.graph_stats.as_ref() {
            if let (Some(name), false) = (label, gs.label_degree_map.is_empty()) {
                gs.label_degree_map
                    .get(name)
                    .copied()
                    .unwrap_or_else(|| gs.avg_out_degree * gs.label_selectivity(Some(name)))
            } else {
                gs.avg_out_degree * gs.label_selectivity(label)
            }
        } else {
            (n * 0.1).min(10.0)
        };

        let hops_f = hops.max(1) as f64;
        let mut result = n.min(branching.powf(hops_f));

        if let (Some(tf), Some(gs)) = (temporal_filter, self.graph_stats.as_ref()) {
            result *= self.temporal_selectivity(tf, gs);
        }
        result
    }

    /// Pattern match cardinality using graph statistics (Theorem 6.3.2,
    /// Paper 2). Mirrors `_estimate_pattern_match`.
    fn estimate_pattern_match(&self, pattern: &GraphPatternIR, n: f64) -> f64 {
        let k = pattern.vertex_patterns.len();
        let e = pattern.edge_patterns.len();

        if let Some(gs) = self.graph_stats.as_ref() {
            let nv = if gs.num_vertices > 0 {
                gs.num_vertices as f64
            } else {
                n
            };

            // Random-walk sampling for large graphs (Section 6.3, Paper 2).
            if nv > 10_000.0 && self.graph_store.is_some() {
                let sampled = self.sample_graph_cardinality(pattern, 100);
                if sampled >= 0.0 {
                    return sampled.max(1.0);
                }
            }

            let density = gs.edge_density();

            let mut label_sel = 1.0;
            for ep in &pattern.edge_patterns {
                label_sel *= gs.label_selectivity(ep.label.as_deref());
            }

            let mut vertex_sel = 1.0;
            if !gs.vertex_label_counts.is_empty() {
                for vp in &pattern.vertex_patterns {
                    if let Some(label) = vp.label.as_deref() {
                        if let Some(vlc) = gs.vertex_label_counts.get(label) {
                            vertex_sel *= if nv > 0.0 { *vlc as f64 / nv } else { 1.0 };
                        }
                    }
                }
            }

            let estimate = nv.powi(k as i32) * density.powi(e as i32) * label_sel * vertex_sel;
            return nv.min(estimate).max(1.0);
        }

        n.min(n.powf(1.5))
    }

    fn estimate_temporal_pattern_match(
        &self,
        pattern: &GraphPatternIR,
        temporal_filter: Option<&TemporalFilterIR>,
        n: f64,
    ) -> f64 {
        let k = pattern.vertex_patterns.len();
        let e = pattern.edge_patterns.len();

        if let Some(gs) = self.graph_stats.as_ref() {
            let nv = if gs.num_vertices > 0 {
                gs.num_vertices as f64
            } else {
                n
            };
            let density = gs.edge_density();

            let mut label_sel = 1.0;
            for ep in &pattern.edge_patterns {
                label_sel *= gs.label_selectivity(ep.label.as_deref());
            }

            let mut estimate = nv.powi(k as i32) * density.powi(e as i32) * label_sel;
            estimate = nv.min(estimate).max(1.0);

            if let Some(tf) = temporal_filter {
                estimate *= self.temporal_selectivity(tf, gs);
            }
            return estimate;
        }

        let mut estimate = n.min(n.powf(1.5));
        if let (Some(tf), Some(gs)) = (temporal_filter, self.graph_stats.as_ref()) {
            estimate *= self.temporal_selectivity(tf, gs);
        }
        estimate
    }

    /// RPQ cardinality using graph statistics (Theorem 6.3.2, Paper
    /// 2). The Rust port estimates `|R|` (NFA size) directly from the
    /// expression source string by counting label-bearing tokens.
    fn estimate_rpq(&self, rpq_source: &str, n: f64) -> f64 {
        if let Some(gs) = self.graph_stats.as_ref() {
            let nv = if gs.num_vertices > 0 {
                gs.num_vertices as f64
            } else {
                n
            };
            let density = gs.edge_density();
            let r_size = rpq_label_count(rpq_source).max(1) as f64;
            let estimate = nv.powi(2) * r_size * density;
            return nv.min(estimate).max(1.0);
        }
        n.min(n.powf(1.5))
    }

    /// Estimate vector selectivity based on threshold (Paper 1,
    /// Section 5.3). Mirrors `_vector_selectivity`.
    fn vector_selectivity(threshold: f32) -> f64 {
        if threshold >= 0.9 {
            return 0.01;
        }
        if threshold >= 0.7 {
            return 0.05;
        }
        if threshold >= 0.5 {
            return 0.1;
        }
        0.2
    }

    fn estimate_join_side(&self, side: &OperatorTree, stats: &IndexStats, _n: f64) -> f64 {
        self.estimate(side, stats)
    }

    /// Random-walk sampling. Returns `-1.0` when the graph store is
    /// unavailable, mirroring Python's sentinel.
    fn sample_graph_cardinality(&self, pattern: &GraphPatternIR, sample_size: usize) -> f64 {
        let Some(store) = self.graph_store.as_ref() else {
            return -1.0;
        };
        let vertex_ids = store.vertex_ids();
        if vertex_ids.is_empty() {
            return 0.0;
        }
        let k = pattern.vertex_patterns.len();
        if k == 0 {
            return 0.0;
        }

        let n = vertex_ids.len();
        let rng = XorShiftRng::new(0xDEAD_BEEF);
        let mut successes = 0_usize;

        for _ in 0..sample_size {
            let start = vertex_ids[rng.bounded(n)];
            let vp0 = &pattern.vertex_patterns[0];
            if !vp0
                .constraints
                .iter()
                .all(|c| store.vertex_satisfies(start, c))
            {
                continue;
            }

            let mut assignment: BTreeMap<String, u64> = BTreeMap::new();
            assignment.insert(vp0.variable.clone(), start);
            let mut valid = true;

            for vi in 1..k {
                let vp = &pattern.vertex_patterns[vi];
                let mut neighbor_found = false;
                for ep in &pattern.edge_patterns {
                    if ep.target_var != vp.variable {
                        continue;
                    }
                    let Some(src_id) = assignment.get(&ep.source_var).copied() else {
                        continue;
                    };
                    let edges = store.outgoing_edges(src_id);
                    let mut candidates: Vec<u64> = Vec::new();
                    for edge in edges {
                        if let Some(label) = &ep.label {
                            if &edge.label != label {
                                continue;
                            }
                        }
                        if vp
                            .constraints
                            .iter()
                            .all(|c| store.vertex_satisfies(edge.target_id, c))
                        {
                            candidates.push(edge.target_id);
                        }
                    }
                    if !candidates.is_empty() {
                        let picked = candidates[rng.bounded(candidates.len())];
                        assignment.insert(vp.variable.clone(), picked);
                        neighbor_found = true;
                        break;
                    }
                }
                if !neighbor_found {
                    valid = false;
                    break;
                }
            }

            if valid && assignment.len() == k {
                successes += 1;
            }
        }

        let success_rate = successes as f64 / sample_size as f64;
        success_rate * (n as f64).powi(k as i32)
    }

    fn temporal_selectivity(&self, filter: &TemporalFilterIR, gs: &GraphStats) -> f64 {
        let (Some(min_ts), Some(max_ts)) = (gs.min_timestamp, gs.max_timestamp) else {
            return 1.0;
        };
        let total_range = max_ts - min_ts;
        if total_range <= 0.0 {
            return 1.0;
        }
        if filter.timestamp.is_some() {
            return (1.0 / total_range).min(1.0);
        }
        if let Some((lo, hi)) = filter.time_range {
            let span = hi - lo;
            return (span / total_range).min(1.0);
        }
        1.0
    }

    /// Join cardinality `|L1| * |L2| / |dom(f)|` (Definition 6.2.3).
    pub fn estimate_join(&self, left_card: f64, right_card: f64, domain_size: f64) -> f64 {
        if domain_size <= 0.0 {
            return 0.0;
        }
        (left_card * right_card) / domain_size
    }

    // -----------------------------------------------------------------
    // Filter selectivity (operator-tree surface)
    // -----------------------------------------------------------------

    fn filter_selectivity(&self, field: &str, predicate: &Predicate, _n: f64) -> f64 {
        let Some(cs) = self.column_stats.get(field) else {
            return 0.5;
        };
        if cs.distinct_count == 0 {
            return 0.5;
        }
        let ndv = cs.distinct_count;
        let mut selectivity = match predicate {
            Predicate::Equals(target) => Self::equality_selectivity(cs, target, ndv),
            Predicate::NotEquals(target) => 1.0 - Self::equality_selectivity(cs, target, ndv),
            Predicate::InSet(values) => values
                .iter()
                .map(|v| Self::equality_selectivity(cs, v, ndv))
                .sum::<f64>()
                .min(1.0),
            Predicate::Between { low, high } => self.range_selectivity_for(cs, low, high),
            Predicate::GreaterThan(target) | Predicate::GreaterThanOrEqual(target) => {
                self.gt_selectivity(cs, target)
            }
            Predicate::LessThan(target) | Predicate::LessThanOrEqual(target) => {
                self.lt_selectivity(cs, target)
            }
            Predicate::IsNull => {
                if cs.row_count > 0 {
                    cs.null_count as f64 / cs.row_count as f64
                } else {
                    0.05
                }
            }
            Predicate::IsNotNull => {
                let null_frac = if cs.row_count > 0 {
                    cs.null_count as f64 / cs.row_count as f64
                } else {
                    0.05
                };
                1.0 - null_frac
            }
        };

        // Entropy-based lower bound.
        if cs.distinct_count > 1 {
            let h = column_entropy(cs);
            if h > 0.0 {
                let min_sel = 1.0 / 2.0_f64.powf(h);
                selectivity = selectivity.max(min_sel);
            }
        }
        selectivity.clamp(0.0, 1.0)
    }

    fn equality_selectivity(cs: &ColumnStats, target: &Value, ndv: u64) -> f64 {
        for (mcv, freq) in cs.mcv_values.iter().zip(cs.mcv_frequencies.iter()) {
            if mcv == target {
                return *freq;
            }
        }
        if ndv > 0 {
            1.0 / ndv as f64
        } else {
            1.0
        }
    }

    fn histogram_fraction(boundaries: &[Value], low: &Value, high: &Value) -> f64 {
        if boundaries.len() < 2 {
            return 0.5;
        }
        let n_buckets = (boundaries.len() - 1) as f64;
        let mut overlapping = 0.0;
        for i in 0..(boundaries.len() - 1) {
            let b_low = &boundaries[i];
            let b_high = &boundaries[i + 1];
            if compare_values(high, b_low).is_lt() || compare_values(low, b_high).is_gt() {
                continue;
            }
            if compare_values(low, b_low).is_le() && compare_values(high, b_high).is_ge() {
                overlapping += 1.0;
                continue;
            }
            let (Some(lo_f), Some(hi_f), Some(b_lo_f), Some(b_hi_f)) = (
                value_as_f64(low),
                value_as_f64(high),
                value_as_f64(b_low),
                value_as_f64(b_high),
            ) else {
                overlapping += 1.0;
                continue;
            };
            let b_span = b_hi_f - b_lo_f;
            if b_span <= 0.0 {
                overlapping += 1.0;
                continue;
            }
            let clamp_lo = lo_f.max(b_lo_f);
            let clamp_hi = hi_f.min(b_hi_f);
            overlapping += (clamp_hi - clamp_lo) / b_span;
        }
        (overlapping / n_buckets).clamp(0.0, 1.0)
    }

    fn range_selectivity_for(&self, cs: &ColumnStats, low: &Value, high: &Value) -> f64 {
        if !cs.histogram.is_empty() {
            return Self::histogram_fraction(&cs.histogram, low, high);
        }
        if let (Some(min_v), Some(max_v)) = (cs.min_value.as_ref(), cs.max_value.as_ref()) {
            if let (Some(lo), Some(hi), Some(mn), Some(mx)) = (
                value_as_f64(low),
                value_as_f64(high),
                value_as_f64(min_v),
                value_as_f64(max_v),
            ) {
                let span = mx - mn;
                if span > 0.0 {
                    return ((hi - lo) / span).clamp(0.0, 1.0);
                }
            }
        }
        0.25
    }

    fn gt_selectivity(&self, cs: &ColumnStats, target: &Value) -> f64 {
        if let Some(last) = cs.histogram.last() {
            return Self::histogram_fraction(&cs.histogram, target, last);
        }
        if let (Some(min_v), Some(max_v)) = (cs.min_value.as_ref(), cs.max_value.as_ref()) {
            if let (Some(t), Some(mn), Some(mx)) = (
                value_as_f64(target),
                value_as_f64(min_v),
                value_as_f64(max_v),
            ) {
                let span = mx - mn;
                if span > 0.0 {
                    return ((mx - t) / span).max(0.0);
                }
            }
        }
        1.0 / 3.0
    }

    fn lt_selectivity(&self, cs: &ColumnStats, target: &Value) -> f64 {
        if let Some(first) = cs.histogram.first() {
            return Self::histogram_fraction(&cs.histogram, first, target);
        }
        if let (Some(min_v), Some(max_v)) = (cs.min_value.as_ref(), cs.max_value.as_ref()) {
            if let (Some(t), Some(mn), Some(mx)) = (
                value_as_f64(target),
                value_as_f64(min_v),
                value_as_f64(max_v),
            ) {
                let span = mx - mn;
                if span > 0.0 {
                    return ((t - mn) / span).max(0.0);
                }
            }
        }
        1.0 / 3.0
    }
}

// ---------------------------------------------------------------------
// Information-theoretic helpers (Paper 1, Section 7).
// ---------------------------------------------------------------------

/// Estimate column entropy from MCV frequencies, equi-depth histogram,
/// or distinct count. Mirrors `_column_entropy`.
pub fn column_entropy(cs: &ColumnStats) -> f64 {
    let ndv = cs.distinct_count;
    if ndv <= 1 {
        return 0.0;
    }

    if !cs.mcv_frequencies.is_empty() {
        let mut entropy = 0.0;
        let sum: f64 = cs.mcv_frequencies.iter().sum();
        let remaining = (1.0 - sum).max(0.0);
        for freq in &cs.mcv_frequencies {
            if *freq > 0.0 {
                entropy -= freq * freq.log2();
            }
        }
        let remaining_ndv = (ndv as i64 - cs.mcv_frequencies.len() as i64).max(1);
        if remaining > 0.0 && remaining_ndv > 0 {
            let p = remaining / remaining_ndv as f64;
            if p > 0.0 {
                entropy -= remaining * p.log2();
            }
        }
        return entropy.max(0.0);
    }

    if cs.histogram.len() > 1 {
        let num_buckets = (cs.histogram.len() - 1) as f64;
        if cs.row_count > 0 && num_buckets > 0.0 {
            let p = 1.0 / num_buckets;
            let entropy = -num_buckets * p * p.log2();
            return entropy.max(0.0);
        }
    }

    (ndv as f64).log2()
}

/// Estimate mutual information `I(X;Y) = H(X) + H(Y) - H(X,Y)`. Mirrors
/// `_mutual_information_estimate`.
pub fn mutual_information_estimate(
    cs_x: &ColumnStats,
    cs_y: &ColumnStats,
    joint_selectivity: f64,
) -> f64 {
    let h_x = column_entropy(cs_x);
    let h_y = column_entropy(cs_y);
    if joint_selectivity <= 0.0 {
        return 0.0;
    }
    let ndv_x = cs_x.distinct_count.max(1);
    let ndv_y = cs_y.distinct_count.max(1);
    let independent = (ndv_x as f64) * (ndv_y as f64);
    let effective = (independent * joint_selectivity).max(1.0);
    let h_joint = effective.max(1.0).log2();
    (h_x + h_y - h_joint).max(0.0)
}

/// Information-theoretic lower bound on intersection cardinality.
pub fn entropy_cardinality_lower_bound(n: f64, entropies: &[f64]) -> f64 {
    if entropies.is_empty() || n <= 0.0 {
        return 1.0;
    }
    let total: f64 = entropies.iter().sum();
    let lb = n * 2.0_f64.powf(-total);
    lb.max(1.0)
}

// ---------------------------------------------------------------------
// AST helpers (shared with `selectivity`)
// ---------------------------------------------------------------------

fn column_of(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Column(c) => Some(c.as_str()),
        Expr::QualifiedColumn { column, .. } => Some(column.as_str()),
        _ => None,
    }
}

fn literal_of(expr: &Expr) -> Option<&Value> {
    match expr {
        Expr::Literal(v) => Some(v),
        _ => None,
    }
}

fn histogram_range_selectivity(stats: &ColumnStats, op: BinaryOp, value: &Value) -> Option<f64> {
    if stats.histogram.is_empty() {
        return None;
    }
    let bucket_count = (stats.histogram.len().saturating_sub(1)).max(1) as f64;
    let position = stats
        .histogram
        .iter()
        .position(|b| compare_values(b, value).is_ge())?;
    let frac = position as f64 / bucket_count;
    let s = match op {
        BinaryOp::Less | BinaryOp::LessEqual => frac,
        BinaryOp::Greater | BinaryOp::GreaterEqual => 1.0 - frac,
        _ => return None,
    };
    Some(s.clamp(0.0, 1.0))
}

fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (Value::Null, Value::Null) => Equal,
        (Value::Null, _) => Less,
        (_, Value::Null) => Greater,
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Equal),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Equal),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Equal),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => Equal,
    }
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Tiny xorshift64 PRNG used by the random-walk sampler. We avoid
/// pulling in `rand` for a single 100-sample loop.
struct XorShiftRng {
    state: Cell<u64>,
}

impl XorShiftRng {
    fn new(seed: u64) -> Self {
        Self {
            state: Cell::new(seed.max(1)),
        }
    }

    fn next_u64(&self) -> u64 {
        let mut s = self.state.get();
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        self.state.set(s);
        s
    }

    /// Uniform random index in `0..bound` (`bound` must be >= 1).
    fn bounded(&self, bound: usize) -> usize {
        if bound <= 1 {
            return 0;
        }
        (self.next_u64() as usize) % bound
    }
}

/// Count label-bearing tokens in an RPQ expression source. Mirrors
/// `uqa.planner.cost_model._expr_label_count`: every alphanumeric
/// identifier counts as one label, every Kleene operator (`*`, `+`,
/// `?`) doubles the contribution to approximate NFA expansion.
fn rpq_label_count(source: &str) -> usize {
    let mut labels = 0_usize;
    let mut in_ident = false;
    for ch in source.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            if !in_ident {
                labels += 1;
                in_ident = true;
            }
        } else {
            in_ident = false;
            if ch == '*' || ch == '+' || ch == '?' {
                labels = labels.saturating_add(labels);
            }
        }
    }
    labels.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str) -> Expr {
        Expr::Column(name.into())
    }

    fn eq(name: &str, v: i64) -> Expr {
        Expr::Binary {
            op: BinaryOp::Equal,
            lhs: Box::new(col(name)),
            rhs: Box::new(Expr::Literal(Value::Int(v))),
        }
    }

    #[test]
    fn equality_uses_distinct_count() {
        let stats = RelationStats::new(1000).with_column(
            "user_id",
            ColumnStats {
                distinct_count: 250,
                row_count: 1000,
                ..Default::default()
            },
        );
        let est = CardinalityEstimator::new();
        let sel = est.selectivity(&eq("user_id", 7), &stats).raw();
        assert!((sel - (1.0 / 250.0)).abs() < 1e-9);
        assert_eq!(est.estimate_rows(&eq("user_id", 7), &stats), 4);
    }

    #[test]
    fn and_selectivity_multiplies() {
        let stats = RelationStats::new(1000).with_column(
            "uid",
            ColumnStats {
                distinct_count: 100,
                row_count: 1000,
                ..Default::default()
            },
        );
        let est = CardinalityEstimator::new();
        let pred = Expr::And(vec![eq("uid", 1), eq("uid", 2)]);
        let sel = est.selectivity(&pred, &stats).raw();
        assert!((sel - (0.01 * 0.01)).abs() < 1e-9);
    }

    #[test]
    fn or_selectivity_uses_inclusion_exclusion() {
        let stats = RelationStats::new(1000).with_column(
            "uid",
            ColumnStats {
                distinct_count: 10,
                row_count: 1000,
                ..Default::default()
            },
        );
        let est = CardinalityEstimator::new();
        let pred = Expr::Or(vec![eq("uid", 1), eq("uid", 2)]);
        let sel = est.selectivity(&pred, &stats).raw();
        assert!((sel - 0.19).abs() < 1e-9);
    }

    #[test]
    fn term_uses_doc_freq() {
        let stats = IndexStats::new(1000).with_doc_freq("body", "rust", 42);
        let est = CardinalityEstimator::new();
        let op = OperatorTree::Term {
            query: "rust".into(),
            field: Some("body".into()),
        };
        assert_eq!(est.estimate(&op, &stats), 42.0);
    }

    #[test]
    fn vector_threshold_picks_tier() {
        assert_eq!(CardinalityEstimator::vector_selectivity(0.95), 0.01);
        assert_eq!(CardinalityEstimator::vector_selectivity(0.7), 0.05);
        assert_eq!(CardinalityEstimator::vector_selectivity(0.5), 0.1);
        assert_eq!(CardinalityEstimator::vector_selectivity(0.0), 0.2);
    }

    #[test]
    fn complement_subtracts_from_n() {
        let stats = IndexStats::new(100).with_doc_freq("body", "x", 30);
        let est = CardinalityEstimator::new();
        let op = OperatorTree::Complement(Box::new(OperatorTree::Term {
            query: "x".into(),
            field: Some("body".into()),
        }));
        assert!((est.estimate(&op, &stats) - 70.0).abs() < 1e-9);
    }

    #[test]
    fn entropy_lower_bound_clamps_intersection() {
        let mut cols = BTreeMap::new();
        cols.insert(
            "a".to_string(),
            ColumnStats {
                distinct_count: 4,
                row_count: 1000,
                ..Default::default()
            },
        );
        let est = CardinalityEstimator::new().with_column_stats(cols);
        let stats = IndexStats::new(1000);
        let op = OperatorTree::Intersect(vec![
            OperatorTree::Filter {
                field: "a".into(),
                predicate: Predicate::Equals(Value::Int(1)),
                source: None,
            },
            OperatorTree::Filter {
                field: "a".into(),
                predicate: Predicate::Equals(Value::Int(2)),
                source: None,
            },
        ]);
        let result = est.estimate(&op, &stats);
        assert!(result >= 1.0);
    }

    #[test]
    fn label_selectivity_handles_empty_graph() {
        let gs = GraphStats::default();
        assert_eq!(gs.label_selectivity(None), 1.0);
        assert_eq!(gs.label_selectivity(Some("knows")), 1.0);
    }

    #[test]
    fn pattern_match_falls_back_when_no_stats() {
        let est = CardinalityEstimator::new();
        let stats = IndexStats::new(100);
        let pattern = GraphPatternIR {
            vertex_patterns: vec![],
            edge_patterns: vec![],
        };
        let op = OperatorTree::PatternMatch {
            pattern,
            graph: "g".into(),
        };
        let r = est.estimate(&op, &stats);
        assert!(r > 0.0);
    }
}
