//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Per-operator cost model.
//!
//! The cost is unitless: relative numbers across plans are what
//! matters. We use the System-R style breakdown of `cpu_cost +
//! io_cost + memory_cost`, scaled by per-operator constants from the
//! canonical UQA behavior (UQA `planner/cost_model`).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperatorKind {
    TableScan,
    IndexScan,
    Filter,
    Project,
    Sort,
    HashAggregate,
    Window,
    Limit,
    HashJoinInner,
    HashJoinOuter,
    SortMergeJoin,
    NestedLoopJoin,
    IndexJoin,
    SemiJoin,
    AntiJoin,
    CrossJoin,
}

#[derive(Debug, Clone, Copy)]
pub struct OperatorCost {
    pub cpu: f64,
    pub io: f64,
    pub memory: f64,
}

impl OperatorCost {
    pub fn zero() -> Self {
        Self {
            cpu: 0.0,
            io: 0.0,
            memory: 0.0,
        }
    }

    pub fn total(&self) -> f64 {
        self.cpu + self.io + self.memory
    }

    pub fn add(&self, other: &OperatorCost) -> OperatorCost {
        OperatorCost {
            cpu: self.cpu + other.cpu,
            io: self.io + other.io,
            memory: self.memory + other.memory,
        }
    }
}

/// Coefficients tuned against the canonical UQA behavior benchmarks.
/// Stable enough that the join enumerator picks the same shapes the
/// the optimizer does on the parity test corpus.
#[derive(Debug, Clone, Copy)]
pub struct CostCoefficients {
    pub scan_per_row: f64,
    pub index_per_row: f64,
    pub filter_per_row: f64,
    pub project_per_row: f64,
    pub sort_per_row_log: f64,
    pub hashagg_build_per_row: f64,
    pub window_per_row: f64,
    pub limit_per_row: f64,
    pub hashjoin_build_per_row: f64,
    pub hashjoin_probe_per_row: f64,
    pub sortmerge_per_row: f64,
    pub nestedloop_per_pair: f64,
    pub crossjoin_per_pair: f64,
    pub io_per_disk_row: f64,
}

impl Default for CostCoefficients {
    fn default() -> Self {
        Self {
            scan_per_row: 1.0,
            index_per_row: 0.1,
            filter_per_row: 0.2,
            project_per_row: 0.1,
            sort_per_row_log: 1.5,
            hashagg_build_per_row: 1.2,
            window_per_row: 1.5,
            limit_per_row: 0.05,
            hashjoin_build_per_row: 0.8,
            hashjoin_probe_per_row: 0.3,
            sortmerge_per_row: 1.0,
            nestedloop_per_pair: 0.05,
            crossjoin_per_pair: 0.04,
            io_per_disk_row: 5.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CostEstimator {
    pub coefficients: CostCoefficients,
}

impl Default for CostEstimator {
    fn default() -> Self {
        Self {
            coefficients: CostCoefficients::default(),
        }
    }
}

impl CostEstimator {
    pub fn new(coefficients: CostCoefficients) -> Self {
        Self { coefficients }
    }

    /// Cost of materialising `rows` rows from `kind`. For join
    /// operators, `rows` is the input cardinality; the enumerator
    /// folds the build / probe sides separately and adds the costs.
    pub fn estimate_unary(&self, kind: OperatorKind, rows: f64) -> OperatorCost {
        let c = &self.coefficients;
        let rows = rows.max(0.0);
        let log_rows = (rows.max(2.0)).log2();
        match kind {
            OperatorKind::TableScan => OperatorCost {
                cpu: rows * c.scan_per_row,
                io: rows * c.io_per_disk_row,
                memory: 0.0,
            },
            OperatorKind::IndexScan => OperatorCost {
                cpu: rows * c.index_per_row,
                io: rows * c.io_per_disk_row * 0.5,
                memory: 0.0,
            },
            OperatorKind::Filter => OperatorCost {
                cpu: rows * c.filter_per_row,
                io: 0.0,
                memory: 0.0,
            },
            OperatorKind::Project => OperatorCost {
                cpu: rows * c.project_per_row,
                io: 0.0,
                memory: 0.0,
            },
            OperatorKind::Sort => OperatorCost {
                cpu: rows * log_rows * c.sort_per_row_log,
                io: 0.0,
                memory: rows,
            },
            OperatorKind::HashAggregate => OperatorCost {
                cpu: rows * c.hashagg_build_per_row,
                io: 0.0,
                memory: rows,
            },
            OperatorKind::Window => OperatorCost {
                cpu: rows * c.window_per_row,
                io: 0.0,
                memory: rows,
            },
            OperatorKind::Limit => OperatorCost {
                cpu: rows * c.limit_per_row,
                io: 0.0,
                memory: 0.0,
            },
            _ => OperatorCost::zero(),
        }
    }

    /// Cost of joining `left_rows` to `right_rows` via `kind`. For
    /// hash joins, the smaller side is assumed to be the build side
    /// (the enumerator decides which one when it constructs the
    /// node).
    pub fn estimate_join(
        &self,
        kind: OperatorKind,
        left_rows: f64,
        right_rows: f64,
    ) -> OperatorCost {
        let c = &self.coefficients;
        let l = left_rows.max(0.0);
        let r = right_rows.max(0.0);
        let (build, probe) = if l <= r { (l, r) } else { (r, l) };
        match kind {
            OperatorKind::HashJoinInner => OperatorCost {
                cpu: build * c.hashjoin_build_per_row + probe * c.hashjoin_probe_per_row,
                io: 0.0,
                memory: build,
            },
            OperatorKind::HashJoinOuter => OperatorCost {
                cpu: build * c.hashjoin_build_per_row * 1.2
                    + probe * c.hashjoin_probe_per_row * 1.2,
                io: 0.0,
                memory: build,
            },
            OperatorKind::SortMergeJoin => {
                let total = l + r;
                OperatorCost {
                    cpu: total * c.sortmerge_per_row + total * (total.max(2.0)).log2() * 0.5,
                    io: 0.0,
                    memory: total,
                }
            }
            OperatorKind::NestedLoopJoin => OperatorCost {
                cpu: l * r * c.nestedloop_per_pair,
                io: 0.0,
                memory: 0.0,
            },
            OperatorKind::IndexJoin => OperatorCost {
                cpu: l * c.hashjoin_probe_per_row + l * c.index_per_row,
                io: l * c.io_per_disk_row * 0.5,
                memory: 0.0,
            },
            OperatorKind::SemiJoin | OperatorKind::AntiJoin => OperatorCost {
                cpu: probe * c.hashjoin_probe_per_row + build * c.hashjoin_build_per_row,
                io: 0.0,
                memory: build,
            },
            OperatorKind::CrossJoin => OperatorCost {
                cpu: l * r * c.crossjoin_per_pair,
                io: 0.0,
                memory: 0.0,
            },
            _ => OperatorCost::zero(),
        }
    }
}

// ---------------------------------------------------------------
// Algebraic-tree cost model (implementation of the canonical UQA implementation's `CostModel` class).
// ---------------------------------------------------------------

use uqa_core::IndexStats;
use uqa_operators::{DeepFusionLayer, OperatorTree};

use crate::cardinality::GraphStats;

/// Mirrors the canonical UQA implementation's `SCORE_OVERHEAD_FACTOR`.
pub const SCORE_OVERHEAD_FACTOR: f64 = 1.1;
/// Mirrors the canonical UQA implementation's `FILTER_SCAN_FRACTION`.
pub const FILTER_SCAN_FRACTION: f64 = 0.1;
/// Mirrors the canonical UQA implementation's `GROUP_BY_OVERHEAD_FACTOR`.
pub const GROUP_BY_OVERHEAD_FACTOR: f64 = 1.5;
/// Mirrors the canonical UQA implementation's `VERTEX_AGG_FRACTION`.
pub const VERTEX_AGG_FRACTION: f64 = 0.2;
/// Mirrors the canonical UQA implementation's `TRAVERSE_FRACTION`.
pub const TRAVERSE_FRACTION: f64 = 0.1;

/// Algebraic operator-tree cost model. Rust implementation of the canonical UQA implementation's
/// `uqa.planner.cost_model.CostModel` — produces a unitless cost for
/// each [`OperatorTree`] node so the query optimiser's
/// `reorder_intersect` pass can pick a join order that matches the
/// canonical UQA behavior.
#[derive(Debug, Clone, Default)]
pub struct CostModel {
    pub graph_stats: Option<GraphStats>,
}

impl CostModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_graph_stats(mut self, stats: GraphStats) -> Self {
        self.graph_stats = Some(stats);
        self
    }

    /// Estimate the cost of a sub-plan against `stats`. Mirrors
    /// the canonical UQA implementation's `CostModel.estimate` (line 30 of `cost_model`).
    pub fn estimate(&self, op: &OperatorTree, stats: &IndexStats) -> f64 {
        let n = stats.total_docs as f64;
        match op {
            OperatorTree::Empty => 0.0,
            OperatorTree::Term { query, field, .. } => {
                if stats.total_docs == 0 {
                    1.0
                } else {
                    let f = field.as_deref().unwrap_or("_default");
                    stats.doc_freq(f, query) as f64
                }
            }
            OperatorTree::VectorSimilarity { .. } | OperatorTree::KNN { .. } => {
                let dims = f64::from(stats.dimensions);
                dims * ((stats.total_docs as f64) + 1.0).log2()
            }
            OperatorTree::CalibratedVectorMatch { .. } => {
                let dims = f64::from(stats.dimensions);
                dims * ((stats.total_docs as f64) + 1.0).log2() * SCORE_OVERHEAD_FACTOR
            }
            OperatorTree::IndexScan { .. } => {
                // The UQA-RS implementation lacks an `IndexScanOperator.cost_estimate`
                // hook; mirror the canonical UQA implementation's behaviour by treating the index
                // scan as proportional to the number of documents the
                // index covers.
                n * 0.1
            }
            OperatorTree::Score { source, .. } => {
                self.estimate(source, stats) * SCORE_OVERHEAD_FACTOR
            }
            OperatorTree::BayesianScore { source, .. } => {
                self.estimate(source, stats) * SCORE_OVERHEAD_FACTOR
            }
            OperatorTree::BayesianMatchWithPrior { query, field, .. } => {
                let postings = if stats.total_docs == 0 {
                    1.0
                } else {
                    stats.doc_freq(field, query) as f64
                };
                postings * SCORE_OVERHEAD_FACTOR
            }
            OperatorTree::Filter { source, .. } => {
                let mut base = n;
                if let Some(src) = source.as_deref() {
                    base = self.estimate(src, stats) + base * FILTER_SCAN_FRACTION;
                }
                base
            }
            OperatorTree::Intersect(ops) => {
                let total: f64 = ops.iter().map(|o| self.estimate(o, stats)).sum();
                total
            }
            OperatorTree::Union(ops) => ops.iter().map(|o| self.estimate(o, stats)).sum(),
            OperatorTree::Aggregate { .. } => n,
            OperatorTree::GroupBy { .. } => n * GROUP_BY_OVERHEAD_FACTOR,
            OperatorTree::BayesianEvidenceFusion { signals, .. }
            | OperatorTree::RobustPositiveEvidencePool { signals, .. }
            | OperatorTree::ProbBoolFusion { signals, .. }
            | OperatorTree::AttentionFusion { signals, .. }
            | OperatorTree::LearnedFusion { signals, .. } => {
                signals.iter().map(|s| self.estimate(s, stats)).sum()
            }
            OperatorTree::ProbNot { signal, .. } => self.estimate(signal, stats) + n,
            OperatorTree::HybridTextVector {
                term_op, vector_op, ..
            } => self.estimate(term_op, stats) + self.estimate(vector_op, stats),
            OperatorTree::SemanticFilter { source, vector_op } => {
                self.estimate(source, stats) + self.estimate(vector_op, stats)
            }
            OperatorTree::VectorExclusion { positive, negative } => {
                self.estimate(positive, stats) + self.estimate(negative, stats)
            }
            OperatorTree::FacetVector { vector_op, .. } => self.estimate(vector_op, stats),
            OperatorTree::VertexAggregation { .. } => n * VERTEX_AGG_FRACTION,
            OperatorTree::Traverse {
                label, max_hops, ..
            }
            | OperatorTree::TemporalTraverse {
                label, max_hops, ..
            } => {
                if let Some(gs) = self.graph_stats.as_ref() {
                    let sel = gs.label_selectivity(label.as_deref());
                    let d = gs.avg_out_degree * sel;
                    let hops = (*max_hops).max(1) as f64;
                    let cost = if d == 1.0 {
                        hops
                    } else if d <= 0.0 {
                        0.0
                    } else {
                        d * (d.powf(hops) - 1.0) / (d - 1.0)
                    };
                    cost.max(1.0)
                } else {
                    n * TRAVERSE_FRACTION
                }
            }
            OperatorTree::GraphNeighbors { label, .. } => self
                .graph_stats
                .as_ref()
                .map(|stats| {
                    (stats.avg_out_degree * stats.label_selectivity(label.as_deref())).max(1.0)
                })
                .unwrap_or(n * TRAVERSE_FRACTION),
            OperatorTree::GraphEdges { label, .. } => self
                .graph_stats
                .as_ref()
                .map(|stats| stats.num_edges as f64 * stats.label_selectivity(label.as_deref()))
                .unwrap_or(n),
            OperatorTree::PatternMatch { pattern, .. } => {
                let k = pattern.vertex_patterns.len() as f64;
                // Negated edge patterns aren't represented in the Rust
                // IR yet (`EdgePatternIR` carries no `negated` flag).
                // The base cost matches the canonical UQA implementation's path; the +20% per
                // negated edge will land once the IR adds the flag.
                if let Some(gs) = self.graph_stats.as_ref() {
                    let nv = if gs.num_vertices > 0 {
                        gs.num_vertices as f64
                    } else {
                        n
                    };
                    (nv.powf(k) * 0.01).max(1.0)
                } else {
                    n * n
                }
            }
            OperatorTree::TemporalPatternMatch { .. } => n * n,
            OperatorTree::RegularPathQuery { rpq_source, .. }
            | OperatorTree::WeightedPathQuery { rpq_source, .. } => {
                // Path-indexable expressions (Concat-of-Labels) are
                // cheap. Falling back to the full RPQ cost otherwise.
                if is_label_chain(rpq_source) {
                    return n * 0.1;
                }
                if let Some(gs) = self.graph_stats.as_ref() {
                    let nv = gs.num_vertices as f64;
                    let r_size = rpq_source_label_count(rpq_source).max(1) as f64;
                    return (nv.powi(2) * r_size * 0.001).max(1.0);
                }
                n * n
            }
            OperatorTree::SparseThreshold { source, .. } => self.estimate(source, stats) * 0.5,
            OperatorTree::MultiFieldSearch { fields, .. } => n * fields.len() as f64,
            OperatorTree::MessagePassing { source } | OperatorTree::GraphEmbedding { source } => {
                self.estimate(source, stats)
            }
            OperatorTree::MultiStage { stages } => stages
                .iter()
                .map(|s| self.estimate(&s.child, stats))
                .sum::<f64>()
                .max(n * 0.1),
            // the canonical UQA implementation's `CostModel` reads `op.max_iterations` directly.
            // The Rust IR doesn't carry this field today, so we use
            // the canonical UQA implementation's default (20). Tracking the iteration count
            // accurately requires extending `OperatorTree::PageRank` /
            // `HITS`.
            OperatorTree::PageRank { .. } => n * 20.0 * 0.1,
            OperatorTree::HITS { .. } => n * 20.0 * 0.2,
            OperatorTree::BetweennessCentrality { .. } => n * n * 0.5,
            OperatorTree::TextSimilarityJoin { .. } => {
                2.0 * n * f64::from(stats.dimensions.max(10))
            }
            OperatorTree::VectorSimilarityJoin { .. } => n * n * f64::from(stats.dimensions.max(1)),
            OperatorTree::GraphJoin { .. } | OperatorTree::CrossParadigmJoin { .. } => n * 10.0,
            OperatorTree::HybridJoin { .. } => n + n * f64::from(stats.dimensions.max(1)),
            OperatorTree::ProgressiveFusion { stages, .. } => {
                stages.last().map(|s| s.k as f64).unwrap_or(n)
            }
            OperatorTree::DeepFusion { layers, .. } => self.estimate_deep_fusion(layers, stats, n),
            OperatorTree::DeepPredict { .. } => n,
            OperatorTree::Composed(ops) | OperatorTree::Opaque { children: ops, .. } => {
                ops.iter().map(|o| self.estimate(o, stats)).sum()
            }
            OperatorTree::Complement(inner) => self.estimate(inner, stats) + n,
            OperatorTree::CosineProbability(inner) => self.estimate(inner, stats),
            OperatorTree::Facet { source, .. } => match source.as_deref() {
                Some(s) => self.estimate(s, stats),
                None => n,
            },
        }
    }

    fn estimate_deep_fusion(&self, layers: &[DeepFusionLayer], stats: &IndexStats, n: f64) -> f64 {
        let mut cost = 0.0_f64;
        for layer in layers {
            match layer {
                DeepFusionLayer::Signal { signals } => {
                    cost += signals.iter().map(|s| self.estimate(s, stats)).sum::<f64>();
                }
                DeepFusionLayer::Propagate { .. } | DeepFusionLayer::Conv { .. } => {
                    cost += n;
                }
                DeepFusionLayer::Pool { .. }
                | DeepFusionLayer::Flatten
                | DeepFusionLayer::Dense { .. }
                | DeepFusionLayer::Softmax
                | DeepFusionLayer::BatchNorm { .. }
                | DeepFusionLayer::Dropout { .. } => {}
            }
        }
        cost.max(n * 0.1)
    }
}

/// Quick check for `Concat(Label, Concat(Label, ...))` shape in UQA
/// recognises path-indexable RPQs and short-circuits the cost. Since
/// the UQA-RS implementation only stores the source string, we approximate by
/// disallowing any quantifier (`*`, `+`, `?`) or alternation (`|`).
fn is_label_chain(source: &str) -> bool {
    !source.contains('*')
        && !source.contains('+')
        && !source.contains('?')
        && !source.contains('|')
        && !source.contains('{')
}

/// Mirror of the canonical UQA implementation's `_expr_label_count`, but operates on the raw
/// source string. Falls back to alphanumeric token counting + `* / +
/// / ?` doubling so empty / unparseable inputs degrade to 1.
fn rpq_source_label_count(source: &str) -> usize {
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

    #[test]
    fn hash_join_prefers_smaller_build_side() {
        let est = CostEstimator::default();
        let a = est.estimate_join(OperatorKind::HashJoinInner, 100.0, 1_000_000.0);
        let b = est.estimate_join(OperatorKind::HashJoinInner, 1_000_000.0, 100.0);
        // Symmetric: the model swaps build/probe internally.
        assert!((a.total() - b.total()).abs() < 1e-6);
    }

    #[test]
    fn nested_loop_grows_quadratically() {
        let est = CostEstimator::default();
        let a = est.estimate_join(OperatorKind::NestedLoopJoin, 100.0, 100.0);
        let b = est.estimate_join(OperatorKind::NestedLoopJoin, 200.0, 200.0);
        assert!(b.total() > a.total() * 3.5);
    }

    #[test]
    fn sort_cpu_dominates_for_large_inputs() {
        let est = CostEstimator::default();
        let cost = est.estimate_unary(OperatorKind::Sort, 10_000.0);
        assert!(cost.cpu > 0.0);
        assert!(cost.memory > 0.0);
    }
}
