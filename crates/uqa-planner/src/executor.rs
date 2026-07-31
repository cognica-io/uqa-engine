//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Planner-to-physical bridge + plan executor.
//!
//! [`PlanExecutor`] is the planner-side entry point for executing an
//! [`OperatorTree`] through a runtime driver. It records root timing
//! statistics and produces an `EXPLAIN`-style tree string.

use std::time::Instant;

use uqa_core::{GeneralizedPostingList, PostingList};
use uqa_graph::GraphPostingList;
use uqa_operators::{DeepFusionLayer, OperatorTree};

/// Statistics from plan execution. Mirrors
/// `uqa.planner.executor.ExecutionStats`.
#[derive(Debug, Clone, Default)]
pub struct ExecutionStats {
    pub operator_name: String,
    pub elapsed_ms: f64,
    pub result_count: usize,
    pub children: Vec<ExecutionStats>,
}

/// Materialised value produced by a physical [`OperatorTree`] node.
///
/// Most operators preserve one document id per row and therefore emit a
/// [`PostingList`]. Graph operators retain their subgraph side table in a
/// [`GraphPostingList`], while join operators preserve an ordered tuple of
/// document ids in a [`GeneralizedPostingList`]. Keeping the carriers distinct
/// prevents set operations from silently applying ordinary payload precedence
/// to graph metadata or comparing synthetic join enumeration positions.
#[derive(Debug, Clone, PartialEq)]
pub enum OperatorOutput {
    Posting(PostingList),
    Graph(GraphPostingList),
    Generalized(GeneralizedPostingList),
}

impl OperatorOutput {
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Posting(result) => result.len(),
            Self::Graph(result) => result.len(),
            Self::Generalized(result) => result.len(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Posting(result) => result.is_empty(),
            Self::Graph(result) => result.is_empty(),
            Self::Generalized(result) => result.is_empty(),
        }
    }

    #[must_use]
    pub fn as_posting(&self) -> Option<&PostingList> {
        match self {
            Self::Posting(result) => Some(result),
            Self::Graph(_) | Self::Generalized(_) => None,
        }
    }

    #[must_use]
    pub fn as_graph(&self) -> Option<&GraphPostingList> {
        match self {
            Self::Graph(result) => Some(result),
            Self::Posting(_) | Self::Generalized(_) => None,
        }
    }

    #[must_use]
    pub fn as_generalized(&self) -> Option<&GeneralizedPostingList> {
        match self {
            Self::Posting(_) | Self::Graph(_) => None,
            Self::Generalized(result) => Some(result),
        }
    }
}

impl From<PostingList> for OperatorOutput {
    fn from(value: PostingList) -> Self {
        Self::Posting(value)
    }
}

impl From<GraphPostingList> for OperatorOutput {
    fn from(value: GraphPostingList) -> Self {
        Self::Graph(value)
    }
}

impl From<GeneralizedPostingList> for OperatorOutput {
    fn from(value: GeneralizedPostingList) -> Self {
        Self::Generalized(value)
    }
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
    /// Failure produced by the physical runtime while materialising a
    /// node or one of its descendants.
    type Error;

    fn execute_node(&self, op: &OperatorTree) -> Result<OperatorOutput, Self::Error>;
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

    /// Execute `op` and capture stats. Driver failures are returned to
    /// the caller instead of being indistinguishable from an empty
    /// physical result.
    pub fn execute(&mut self, op: &OperatorTree) -> Result<OperatorOutput, D::Error> {
        self.last_stats = None;
        let start = Instant::now();
        let result = self.driver.execute_node(op)?;
        let mut stats = ExecutionStats::new(operator_name(op));
        stats.elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        stats.result_count = result.len();
        self.last_stats = Some(stats);
        Ok(result)
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

/// Stable human-readable name for an [`OperatorTree`] variant.
pub fn operator_name(op: &OperatorTree) -> String {
    match op {
        OperatorTree::Empty => "EmptyOp",
        OperatorTree::Term {
            top_k:
                Some(uqa_operators::TextTopKPlan {
                    strategy: uqa_operators::TextTopKStrategy::Wand,
                    ..
                }),
            ..
        } => "WANDTopKOp",
        OperatorTree::Term {
            top_k:
                Some(uqa_operators::TextTopKPlan {
                    strategy: uqa_operators::TextTopKStrategy::BlockMaxWand,
                    ..
                }),
            ..
        } => "BlockMaxWANDTopKOp",
        OperatorTree::Term { .. } => "TermOp",
        OperatorTree::Filter { .. } => "FilterOp",
        OperatorTree::Facet { .. } => "FacetOp",
        OperatorTree::Score { .. } => "ScoreOp",
        OperatorTree::BayesianScore { .. } => "BayesianScoreQuery",
        OperatorTree::BayesianMatchWithPrior { .. } => "BayesianMatchWithPriorOp",
        OperatorTree::Intersect(_) => "IntersectOp",
        OperatorTree::Union(_) => "UnionOp",
        OperatorTree::Complement(_) => "ComplementOp",
        OperatorTree::Composed(_) => "ComposedOp",
        OperatorTree::EncodeGraphPosting { .. } => "EncodeGraphPostingOp",
        OperatorTree::VectorSimilarity { .. } => "VectorSimOp",
        OperatorTree::KNN { .. } => "KNNOp",
        OperatorTree::CalibratedVectorMatch { .. } => "CalibratedVectorMatchOp",
        OperatorTree::CosineProbability(_) => "CosineProbabilityOp",
        OperatorTree::BayesianEvidenceFusion { .. } => "BayesianEvidenceFusion",
        OperatorTree::RobustPositiveEvidencePool { .. } => "RobustPositiveEvidencePool",
        OperatorTree::ProbBoolFusion { .. } => "ProbBoolFusion",
        OperatorTree::ProbNot { .. } => "ProbNot",
        OperatorTree::AttentionFusion { .. } => "AttentionFusion",
        OperatorTree::LearnedFusion { .. } => "LearnedFusion",
        OperatorTree::SparseThreshold { .. } => "SparseThreshold",
        OperatorTree::Traverse { .. } => "TraverseOp",
        OperatorTree::GraphNeighbors { .. } => "GraphNeighborsOp",
        OperatorTree::GraphEdges { .. } => "GraphEdgesOp",
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
        OperatorTree::DeepPredict { .. } => "DeepPredictOp",
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
            top_k,
        } => {
            lines.push(format!(
                "{prefix}TermOp(term={query:?}, field={field:?}, scoring={scoring:?}, top_k={top_k:?})"
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
        OperatorTree::BayesianScore { source, field } => {
            lines.push(format!("{prefix}BayesianScoreQuery(field={field:?})"));
            explain_recursive(source, lines, indent + 1);
        }
        OperatorTree::Filter { field, source, .. } => {
            lines.push(format!("{prefix}FilterOp(field={field:?})"));
            if let Some(src) = source {
                explain_recursive(src, lines, indent + 1);
            }
        }
        OperatorTree::BayesianEvidenceFusion { signals, base_rate } => {
            lines.push(format!(
                "{prefix}BayesianEvidenceFusion(base_rate={base_rate:?}, signals={})",
                signals.len()
            ));
            for signal in signals {
                explain_recursive(signal, lines, indent + 1);
            }
        }
        OperatorTree::RobustPositiveEvidencePool { signals, alpha, .. } => {
            lines.push(format!(
                "{prefix}RobustPositiveEvidencePool(alpha={alpha}, signals={})",
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
        OperatorTree::EncodeGraphPosting { source } => {
            lines.push(format!("{prefix}EncodeGraphPosting"));
            explain_recursive(source, lines, indent + 1);
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
                    DeepFusionLayer::Propagate {
                        edge_label,
                        aggregation,
                        direction,
                    } => {
                        lines.push(format!(
                            "{prefix}  Layer {i} (propagate={edge_label:?}, aggregation={aggregation:?}, direction={direction:?}):"
                        ));
                    }
                    DeepFusionLayer::Conv {
                        edge_label,
                        hop_weights,
                        direction,
                    } => {
                        lines.push(format!(
                            "{prefix}  Layer {i} (convolve={edge_label:?}, hop_weights={hop_weights:?}, direction={direction:?}):"
                        ));
                    }
                    DeepFusionLayer::Pool {
                        edge_label,
                        pool_size,
                        method,
                        direction,
                    } => {
                        lines.push(format!(
                            "{prefix}  Layer {i} (pool={edge_label:?}, size={pool_size}, method={method:?}, direction={direction:?}):"
                        ));
                    }
                    DeepFusionLayer::Flatten => {
                        lines.push(format!("{prefix}  Layer {i} (flatten):"));
                    }
                    DeepFusionLayer::Softmax => {
                        lines.push(format!("{prefix}  Layer {i} (softmax):"));
                    }
                    DeepFusionLayer::Dense {
                        output_channels,
                        input_channels,
                        ..
                    } => {
                        lines.push(format!(
                            "{prefix}  Layer {i} (dense, input={input_channels}, output={output_channels}):"
                        ));
                    }
                    DeepFusionLayer::BatchNorm { epsilon } => {
                        lines.push(format!(
                            "{prefix}  Layer {i} (batch_norm, epsilon={epsilon}):"
                        ));
                    }
                    DeepFusionLayer::Dropout { probability } => {
                        lines.push(format!(
                            "{prefix}  Layer {i} (dropout, probability={probability}):"
                        ));
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
        type Error = std::convert::Infallible;

        fn execute_node(&self, _op: &OperatorTree) -> Result<OperatorOutput, Self::Error> {
            Ok(PostingList::new().into())
        }
    }

    #[test]
    fn explain_renders_intersect_tree() {
        let op = OperatorTree::Intersect(vec![
            OperatorTree::Term {
                query: "rust".into(),
                field: Some("body".into()),
                scoring: None,
                top_k: None,
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
            top_k: None,
        };
        let driver = EmptyDriver;
        let mut executor = PlanExecutor::new(&driver);
        let _ = executor.execute(&op).expect("infallible driver");
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
            type Error = std::convert::Infallible;

            fn execute_node(&self, _op: &OperatorTree) -> Result<OperatorOutput, Self::Error> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(PostingList::new().into())
            }
        }

        let op = OperatorTree::Intersect(vec![
            OperatorTree::Term {
                query: "rust".into(),
                field: Some("body".into()),
                scoring: None,
                top_k: None,
            },
            OperatorTree::Term {
                query: "search".into(),
                field: Some("body".into()),
                scoring: None,
                top_k: None,
            },
        ]);
        let driver = CountingDriver {
            calls: AtomicUsize::new(0),
        };
        let mut executor = PlanExecutor::new(&driver);

        let _ = executor.execute(&op).expect("infallible driver");

        assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn execute_propagates_driver_errors() {
        struct FailingDriver;

        impl OperatorTreeDriver for FailingDriver {
            type Error = &'static str;

            fn execute_node(&self, _op: &OperatorTree) -> Result<OperatorOutput, Self::Error> {
                Err("storage failure")
            }
        }

        let driver = FailingDriver;
        let mut executor = PlanExecutor::new(&driver);
        let error = executor
            .execute(&OperatorTree::Empty)
            .expect_err("driver failure must be returned");

        assert_eq!(error, "storage failure");
        assert!(executor.last_stats().is_none());
    }
}
