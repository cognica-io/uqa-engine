//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Planner-to-physical bridge + plan executor.
//!
//! [`PlannedQuery`] wraps the chosen [`JoinPlan`] alongside the
//! optimised [`SelectStmt`] so the engine can hand the bundle to the
//! execution layer.
//!
//! [`PlanExecutor`] is the planner-side entry point for executing an
//! [`OperatorTree`] through a runtime driver. It records root timing
//! statistics and produces an `EXPLAIN`-style tree string.

use std::time::Instant;

use uqa_core::PostingList;
use uqa_operators::{DeepFusionLayer, OperatorTree};
use uqa_sql::ast::SelectStmt;

use crate::join_enumerator::JoinPlan;

#[derive(Debug, Clone)]
pub struct PlannedQuery {
    pub stmt: SelectStmt,
    pub join_plan: Option<JoinPlan>,
}

impl PlannedQuery {
    pub fn new(stmt: SelectStmt) -> Self {
        Self {
            stmt,
            join_plan: None,
        }
    }

    pub fn with_join_plan(mut self, plan: JoinPlan) -> Self {
        self.join_plan = Some(plan);
        self
    }
}

/// Statistics from plan execution. Mirrors
/// `uqa.planner.executor.ExecutionStats`.
#[derive(Debug, Clone, Default)]
pub struct ExecutionStats {
    pub operator_name: String,
    pub elapsed_ms: f64,
    pub result_count: usize,
    pub children: Vec<ExecutionStats>,
}

impl ExecutionStats {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            operator_name: name.into(),
            elapsed_ms: 0.0,
            result_count: 0,
            children: Vec::new(),
        }
    }
}

/// Driver that knows how to execute an [`OperatorTree`] node. The
/// engine implements this trait (it owns the per-operator dispatch,
/// child execution, and the runtime context); the planner-side
/// `PlanExecutor` only wraps root timing and stats collection.
pub trait OperatorTreeDriver {
    fn execute_node(&self, op: &OperatorTree) -> PostingList;
}

/// Executor wrapper for [`OperatorTree`].
pub struct PlanExecutor<'d, D: OperatorTreeDriver> {
    pub driver: &'d D,
    last_stats: Option<ExecutionStats>,
}

impl<'d, D: OperatorTreeDriver> PlanExecutor<'d, D> {
    pub fn new(driver: &'d D) -> Self {
        Self {
            driver,
            last_stats: None,
        }
    }

    /// Execute `op` and capture stats. The result is the [`PostingList`]
    /// produced by the root node.
    pub fn execute(&mut self, op: &OperatorTree) -> PostingList {
        let start = Instant::now();
        let result = self.driver.execute_node(op);
        let mut stats = ExecutionStats::new(operator_name(op));
        stats.elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        stats.result_count = result.len();
        self.last_stats = Some(stats);
        result
    }

    pub fn last_stats(&self) -> Option<&ExecutionStats> {
        self.last_stats.as_ref()
    }

    pub fn explain(&self, op: &OperatorTree) -> String {
        let mut lines: Vec<String> = Vec::new();
        explain_recursive(op, &mut lines, 0);
        lines.join("\n")
    }
}

fn operator_name(op: &OperatorTree) -> String {
    match op {
        OperatorTree::Empty => "EmptyOp",
        OperatorTree::Term { .. } => "TermOp",
        OperatorTree::Filter { .. } => "FilterOp",
        OperatorTree::Facet { .. } => "FacetOp",
        OperatorTree::Score { .. } => "ScoreOp",
        OperatorTree::Intersect(_) => "IntersectOp",
        OperatorTree::Union(_) => "UnionOp",
        OperatorTree::Complement(_) => "ComplementOp",
        OperatorTree::Composed(_) => "ComposedOp",
        OperatorTree::VectorSimilarity { .. } => "VectorSimOp",
        OperatorTree::KNN { .. } => "KNNOp",
        OperatorTree::CosineProbability(_) => "CosineProbabilityOp",
        OperatorTree::LogOddsFusion { .. } => "LogOddsFusion",
        OperatorTree::ProbBoolFusion { .. } => "ProbBoolFusion",
        OperatorTree::ProbNot { .. } => "ProbNot",
        OperatorTree::AttentionFusion { .. } => "AttentionFusion",
        OperatorTree::LearnedFusion { .. } => "LearnedFusion",
        OperatorTree::SparseThreshold { .. } => "SparseThreshold",
        OperatorTree::Traverse { .. } => "TraverseOp",
        OperatorTree::PatternMatch { .. } => "PatternMatchOp",
        OperatorTree::RegularPathQuery { .. } => "RPQOp",
        OperatorTree::GraphJoin { .. } => "GraphJoinOp",
        OperatorTree::IndexScan { .. } => "IndexScanOp",
        OperatorTree::Aggregate { .. } => "AggregateOp",
        OperatorTree::GroupBy { .. } => "GroupByOp",
        OperatorTree::MultiStage { .. } => "MultiStage",
        OperatorTree::MultiFieldSearch { .. } => "MultiFieldSearchOp",
        OperatorTree::HybridTextVector { .. } => "HybridTextVectorOp",
        OperatorTree::SemanticFilter { .. } => "SemanticFilterOp",
        OperatorTree::VectorExclusion { .. } => "VectorExclusionOp",
        OperatorTree::FacetVector { .. } => "FacetVectorOp",
        OperatorTree::VertexAggregation { .. } => "VertexAggregationOp",
        OperatorTree::WeightedPathQuery { .. } => "WeightedPathQueryOp",
        OperatorTree::MessagePassing { .. } => "MessagePassingOp",
        OperatorTree::GraphEmbedding { .. } => "GraphEmbeddingOp",
        OperatorTree::PageRank { .. } => "PageRankOp",
        OperatorTree::HITS { .. } => "HITSOp",
        OperatorTree::BetweennessCentrality { .. } => "BetweennessCentralityOp",
        OperatorTree::TextSimilarityJoin { .. } => "TextSimilarityJoinOp",
        OperatorTree::VectorSimilarityJoin { .. } => "VectorSimilarityJoinOp",
        OperatorTree::HybridJoin { .. } => "HybridJoinOp",
        OperatorTree::CrossParadigmJoin { .. } => "CrossParadigmJoinOp",
        OperatorTree::TemporalTraverse { .. } => "TemporalTraverseOp",
        OperatorTree::TemporalPatternMatch { .. } => "TemporalPatternMatchOp",
        OperatorTree::ProgressiveFusion { .. } => "ProgressiveFusionOp",
        OperatorTree::DeepFusion { .. } => "DeepFusion",
        OperatorTree::Opaque { kind, .. } => return kind.clone(),
    }
    .to_string()
}

fn explain_recursive(op: &OperatorTree, lines: &mut Vec<String>, indent: usize) {
    let prefix = "  ".repeat(indent);
    match op {
        OperatorTree::Term {
            query,
            field,
            scoring,
        } => {
            lines.push(format!(
                "{prefix}TermOp(term={query:?}, field={field:?}, scoring={scoring:?})"
            ));
        }
        OperatorTree::VectorSimilarity {
            threshold, field, ..
        } => {
            lines.push(format!(
                "{prefix}VectorSimOp(threshold={threshold}, field={field:?})"
            ));
        }
        OperatorTree::KNN { k, field, .. } => {
            lines.push(format!("{prefix}KNNOp(k={k}, field={field:?})"));
        }
        OperatorTree::IndexScan {
            field, index_name, ..
        } => {
            lines.push(format!(
                "{prefix}IndexScanOp(field={field:?}, index={index_name:?})"
            ));
        }
        OperatorTree::Score {
            query_terms,
            field,
            source,
            ..
        } => {
            lines.push(format!(
                "{prefix}ScoreOp(scorer=Scorer, terms={query_terms:?}, field={field:?})"
            ));
            explain_recursive(source, lines, indent + 1);
        }
        OperatorTree::Filter { field, source, .. } => {
            lines.push(format!("{prefix}FilterOp(field={field:?})"));
            if let Some(src) = source {
                explain_recursive(src, lines, indent + 1);
            }
        }
        OperatorTree::LogOddsFusion { signals, alpha, .. } => {
            lines.push(format!(
                "{prefix}LogOddsFusion(alpha={alpha}, signals={})",
                signals.len()
            ));
            for sig in signals {
                explain_recursive(sig, lines, indent + 1);
            }
        }
        OperatorTree::ProbBoolFusion { signals, mode } => {
            lines.push(format!(
                "{prefix}ProbBoolFusion(mode={mode:?}, signals={})",
                signals.len()
            ));
            for sig in signals {
                explain_recursive(sig, lines, indent + 1);
            }
        }
        OperatorTree::ProbNot { signal, .. } => {
            lines.push(format!("{prefix}ProbNot"));
            explain_recursive(signal, lines, indent + 1);
        }
        OperatorTree::AttentionFusion { signals, .. } => {
            lines.push(format!(
                "{prefix}AttentionFusion(signals={})",
                signals.len()
            ));
            for sig in signals {
                explain_recursive(sig, lines, indent + 1);
            }
        }
        OperatorTree::LearnedFusion { signals, .. } => {
            lines.push(format!("{prefix}LearnedFusion(signals={})", signals.len()));
            for sig in signals {
                explain_recursive(sig, lines, indent + 1);
            }
        }
        OperatorTree::Traverse {
            start_vertex,
            label,
            max_hops,
            ..
        } => {
            lines.push(format!(
                "{prefix}TraverseOp(start={start_vertex}, label={label:?}, hops={max_hops})"
            ));
        }
        OperatorTree::PatternMatch { pattern, .. } => {
            lines.push(format!(
                "{prefix}PatternMatchOp(vertices={}, edges={})",
                pattern.vertex_patterns.len(),
                pattern.edge_patterns.len()
            ));
        }
        OperatorTree::RegularPathQuery { start_vertex, .. } => {
            lines.push(format!("{prefix}RPQOp(start={start_vertex})"));
        }
        OperatorTree::Intersect(ops) => {
            lines.push(format!("{prefix}Intersect"));
            for child in ops {
                explain_recursive(child, lines, indent + 1);
            }
        }
        OperatorTree::Union(ops) => {
            lines.push(format!("{prefix}Union"));
            for child in ops {
                explain_recursive(child, lines, indent + 1);
            }
        }
        OperatorTree::Complement(inner) => {
            lines.push(format!("{prefix}Complement"));
            explain_recursive(inner, lines, indent + 1);
        }
        OperatorTree::Composed(ops) => {
            lines.push(format!("{prefix}Composed"));
            for child in ops {
                explain_recursive(child, lines, indent + 1);
            }
        }
        OperatorTree::SparseThreshold { source, threshold } => {
            lines.push(format!("{prefix}SparseThreshold(threshold={threshold})"));
            explain_recursive(source, lines, indent + 1);
        }
        OperatorTree::MessagePassing { source } => {
            lines.push(format!("{prefix}MessagePassingOp"));
            explain_recursive(source, lines, indent + 1);
        }
        OperatorTree::GraphEmbedding { source } => {
            lines.push(format!("{prefix}GraphEmbeddingOp"));
            explain_recursive(source, lines, indent + 1);
        }
        OperatorTree::MultiStage { stages } => {
            lines.push(format!("{prefix}MultiStage(stages={})", stages.len()));
            for (i, entry) in stages.iter().enumerate() {
                lines.push(format!("{prefix}  Stage {i} (cutoff={:?}):", entry.cutoff));
                explain_recursive(&entry.child, lines, indent + 2);
            }
        }
        OperatorTree::DeepFusion {
            layers,
            alpha,
            gating,
        } => {
            lines.push(format!(
                "{prefix}DeepFusion(layers={}, alpha={alpha}, gating={gating:?})",
                layers.len()
            ));
            for (i, layer) in layers.iter().enumerate() {
                match layer {
                    DeepFusionLayer::Signal { signals } => {
                        lines.push(format!("{prefix}  Layer {i} (signals={}):", signals.len()));
                        for sig in signals {
                            explain_recursive(sig, lines, indent + 2);
                        }
                    }
                    DeepFusionLayer::Propagate { edge_label } => {
                        lines.push(format!("{prefix}  Layer {i} (propagate={edge_label:?}):"));
                    }
                    DeepFusionLayer::Conv => {
                        lines.push(format!("{prefix}  Layer {i} (convolve):"));
                    }
                    DeepFusionLayer::Pool { pool_size } => {
                        lines.push(format!("{prefix}  Layer {i} (pool, size={pool_size}):"));
                    }
                    DeepFusionLayer::Flatten => {
                        lines.push(format!("{prefix}  Layer {i} (flatten):"));
                    }
                    DeepFusionLayer::Softmax => {
                        lines.push(format!("{prefix}  Layer {i} (softmax):"));
                    }
                    DeepFusionLayer::Dense => {
                        lines.push(format!("{prefix}  Layer {i} (dense):"));
                    }
                    DeepFusionLayer::BatchNorm => {
                        lines.push(format!("{prefix}  Layer {i} (batch_norm):"));
                    }
                    DeepFusionLayer::Dropout => {
                        lines.push(format!("{prefix}  Layer {i} (dropout):"));
                    }
                }
            }
        }
        OperatorTree::Opaque { kind, children, .. } => {
            lines.push(format!("{prefix}{kind}"));
            for child in children {
                explain_recursive(child, lines, indent + 1);
            }
        }
        other => {
            lines.push(format!("{prefix}{}", operator_name(other)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct EmptyDriver;
    impl OperatorTreeDriver for EmptyDriver {
        fn execute_node(&self, _op: &OperatorTree) -> PostingList {
            PostingList::new()
        }
    }

    #[test]
    fn explain_renders_intersect_tree() {
        let op = OperatorTree::Intersect(vec![
            OperatorTree::Term {
                query: "rust".into(),
                field: Some("body".into()),
                scoring: None,
            },
            OperatorTree::Filter {
                field: "year".into(),
                predicate: uqa_core::Predicate::Equals(uqa_core::Value::Int(2026)),
                source: None,
            },
        ]);
        let driver = EmptyDriver;
        let executor = PlanExecutor::new(&driver);
        let text = executor.explain(&op);
        assert!(text.contains("Intersect"));
        assert!(text.contains("TermOp"));
        assert!(text.contains("FilterOp"));
    }

    #[test]
    fn execute_collects_timing_stats() {
        let op = OperatorTree::Term {
            query: "x".into(),
            field: Some("body".into()),
            scoring: None,
        };
        let driver = EmptyDriver;
        let mut executor = PlanExecutor::new(&driver);
        let _ = executor.execute(&op);
        let stats = executor.last_stats().expect("stats");
        assert_eq!(stats.operator_name, "TermOp");
        assert_eq!(stats.result_count, 0);
        assert!(stats.elapsed_ms >= 0.0);
    }

    #[test]
    fn execute_delegates_to_driver_once() {
        struct CountingDriver {
            calls: AtomicUsize,
        }

        impl OperatorTreeDriver for CountingDriver {
            fn execute_node(&self, _op: &OperatorTree) -> PostingList {
                self.calls.fetch_add(1, Ordering::SeqCst);
                PostingList::new()
            }
        }

        let op = OperatorTree::Intersect(vec![
            OperatorTree::Term {
                query: "rust".into(),
                field: Some("body".into()),
                scoring: None,
            },
            OperatorTree::Term {
                query: "search".into(),
                field: Some("body".into()),
                scoring: None,
            },
        ]);
        let driver = CountingDriver {
            calls: AtomicUsize::new(0),
        };
        let mut executor = PlanExecutor::new(&driver);

        let _ = executor.execute(&op);

        assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
    }
}
