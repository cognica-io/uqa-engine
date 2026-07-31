//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Operator tree IR for the planner.
//!
//! Mirrors UQA `operators/base` + the concrete operator subclass
//! taxonomy. the canonical UQA implementation's `QueryOptimizer` uses `isinstance` to walk an
//! operator tree and rewrite it; the UQA-RS implementation lifts every concrete
//! operator into a single [`OperatorTree`] enum so the rewriter can
//! pattern-match the same way.
//!
//! The enum is *additive* over the existing trait-object operators:
//! the engine still composes operators through `Arc<dyn Operator>` at
//! runtime, but the planner pre-rewrites an [`OperatorTree`] before
//! handing it to the executor.

#![allow(clippy::large_enum_variant)]

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::{Predicate, Value};

use crate::aggregation::AggregationMonoid;
use crate::base::Direction;

/// Reference to a scorer used by a `Score` node. The optimizer only
/// inspects the field/query-terms of the score node; the scorer is
/// passed through opaquely.
pub type ScorerRef = Arc<dyn uqa_scoring::Scorer>;

/// Reference to an attention fusion model.
pub type AttentionRef = Arc<dyn AttentionFuserDyn>;

/// Reference to a learned fusion model.
pub type LearnedFusionRef = Arc<dyn LearnedFuserDyn>;

/// Function pointer for a vertex predicate (used by graph traverse).
pub type VertexPredicate = Arc<dyn Fn(&uqa_core::Vertex) -> bool + Send + Sync>;

/// Predicate over the accumulated numeric weight of a matching regular path.
pub type PathWeightPredicate = Arc<dyn Fn(f64) -> bool + Send + Sync>;

/// Function pointer for a vertex constraint (used by pattern match).
pub type VertexConstraint = Arc<dyn Fn(&uqa_core::Vertex) -> bool + Send + Sync>;

/// Function pointer for an edge constraint (used by pattern match).
pub type EdgeConstraint = Arc<dyn Fn(&uqa_core::Edge) -> bool + Send + Sync>;

pub trait AttentionFuserDyn: Send + Sync {
    fn validate_inputs(
        &self,
        signal_count: usize,
        query_feature_count: usize,
    ) -> Result<(), &'static str>;
    fn fuse(&self, probs: &[f64], query_features: &[f64]) -> Result<f64, &'static str>;

    fn fuse_batch(
        &self,
        probabilities: &[Vec<f64>],
        query_features: &[f64],
    ) -> Result<Vec<f64>, &'static str> {
        probabilities
            .iter()
            .map(|sample| self.fuse(sample, query_features))
            .collect()
    }

    /// Number of independently trained attention heads represented by this
    /// physical fuser. Exposed as immutable IR metadata for explain/testing.
    fn head_count(&self) -> usize;
    fn normalize(&self) -> bool;
    fn alpha(&self) -> f64;
    fn base_rate(&self) -> Option<f64>;
}

pub trait LearnedFuserDyn: Send + Sync {
    fn validate_inputs(&self, signal_count: usize) -> Result<(), &'static str>;
    fn fuse(&self, probs: &[f64]) -> Result<f64, &'static str>;
}

impl AttentionFuserDyn for uqa_fusion::AttentionFusion {
    fn validate_inputs(
        &self,
        signal_count: usize,
        query_feature_count: usize,
    ) -> Result<(), &'static str> {
        uqa_fusion::AttentionFusion::validate_inputs(self, signal_count, query_feature_count)
    }

    fn fuse(&self, probs: &[f64], query_features: &[f64]) -> Result<f64, &'static str> {
        uqa_fusion::AttentionFusion::fuse(self, probs, query_features)
    }

    fn fuse_batch(
        &self,
        probabilities: &[Vec<f64>],
        query_features: &[f64],
    ) -> Result<Vec<f64>, &'static str> {
        uqa_fusion::AttentionFusion::fuse_batch(self, probabilities, query_features)
    }

    fn head_count(&self) -> usize {
        1
    }

    fn normalize(&self) -> bool {
        self.normalize
    }

    fn alpha(&self) -> f64 {
        self.alpha
    }

    fn base_rate(&self) -> Option<f64> {
        self.base_rate
    }
}

impl AttentionFuserDyn for uqa_fusion::MultiHeadAttentionFusion {
    fn validate_inputs(
        &self,
        signal_count: usize,
        query_feature_count: usize,
    ) -> Result<(), &'static str> {
        uqa_fusion::MultiHeadAttentionFusion::validate_inputs(
            self,
            signal_count,
            query_feature_count,
        )
    }

    fn fuse(&self, probs: &[f64], query_features: &[f64]) -> Result<f64, &'static str> {
        uqa_fusion::MultiHeadAttentionFusion::fuse(self, probs, query_features)
    }

    fn fuse_batch(
        &self,
        probabilities: &[Vec<f64>],
        query_features: &[f64],
    ) -> Result<Vec<f64>, &'static str> {
        uqa_fusion::MultiHeadAttentionFusion::fuse_batch(self, probabilities, query_features)
    }

    fn head_count(&self) -> usize {
        uqa_fusion::MultiHeadAttentionFusion::n_heads(self)
    }

    fn normalize(&self) -> bool {
        uqa_fusion::MultiHeadAttentionFusion::normalize(self)
    }

    fn alpha(&self) -> f64 {
        uqa_fusion::MultiHeadAttentionFusion::alpha(self).unwrap_or(f64::NAN)
    }

    fn base_rate(&self) -> Option<f64> {
        None
    }
}

impl LearnedFuserDyn for uqa_fusion::LearnedFusion {
    fn validate_inputs(&self, signal_count: usize) -> Result<(), &'static str> {
        uqa_fusion::LearnedFusion::validate_inputs(self, signal_count)
    }

    fn fuse(&self, probs: &[f64]) -> Result<f64, &'static str> {
        uqa_fusion::LearnedFusion::fuse(self, probs)
    }
}

/// Single vertex pattern (variable name + accumulated constraints).
#[derive(Clone)]
pub struct VertexPatternIR {
    pub variable: String,
    pub constraints: Vec<VertexConstraint>,
    /// Optional label filter; when `None` any vertex matches.
    pub label: Option<String>,
}

impl std::fmt::Debug for VertexPatternIR {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VertexPatternIR")
            .field("variable", &self.variable)
            .field("constraints_count", &self.constraints.len())
            .field("label", &self.label)
            .finish()
    }
}

#[derive(Clone)]
pub struct EdgePatternIR {
    pub source_var: String,
    pub target_var: String,
    pub label: Option<String>,
    pub constraints: Vec<EdgeConstraint>,
}

impl std::fmt::Debug for EdgePatternIR {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgePatternIR")
            .field("source_var", &self.source_var)
            .field("target_var", &self.target_var)
            .field("label", &self.label)
            .field("constraints_count", &self.constraints.len())
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct GraphPatternIR {
    pub vertex_patterns: Vec<VertexPatternIR>,
    pub edge_patterns: Vec<EdgePatternIR>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbBoolMode {
    And,
    Or,
}

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

/// Neighborhood reduction used by a graph-aware deep-fusion propagation
/// layer. This lives in the algebra crate so the IR does not depend on the ML
/// runtime crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeepFusionAggregation {
    Mean,
    Sum,
    Max,
}

/// Element-wise reduction used by a graph-aware deep-fusion pooling layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeepFusionPoolMethod {
    Average,
    Max,
}

/// Text scoring algorithm used by [`OperatorTree::Term`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TextScoringMode {
    BM25,
    BayesianBM25,
    /// Explicit parameters supplied through the public engine API.
    CustomBM25(uqa_scoring::BM25Params),
    /// Explicit Bayesian calibration supplied through the public engine API.
    CustomBayesianBM25(uqa_scoring::BayesianBM25Params),
}

/// External document prior used by [`OperatorTree::BayesianMatchWithPrior`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalPriorMode {
    Authority,
    Recency,
}

/// Concrete operator tree mirroring `uqa/operators` operator class hierarchy.
#[derive(Clone)]
pub enum OperatorTree {
    /// Empty leaf (no input). The optimizer treats `Intersect([])` and
    /// `Union([])` as empty when checking absorption rules.
    Empty,

    /// `TermOperator(query_string, field)` -- text retrieval primitive.
    Term {
        query: String,
        field: Option<String>,
        /// Bound by SQL function lowering when a caller explicitly
        /// chooses `text_match` or `bayesian_match`. Query-string
        /// parsers leave this unset so they stay syntax-only.
        scoring: Option<TextScoringMode>,
    },
    /// `FilterOperator(field, predicate, source)`.
    Filter {
        field: String,
        predicate: Predicate,
        source: Option<Box<OperatorTree>>,
    },
    /// `FacetOperator(field, source)`.
    Facet {
        field: String,
        source: Option<Box<OperatorTree>>,
    },
    /// `ScoreOperator(scorer, source, query_terms, field)`.
    Score {
        scorer: ScorerRef,
        source: Box<OperatorTree>,
        query_terms: Vec<String>,
        field: String,
    },
    /// Lucene-style `BayesianScoreQuery(source)`. The source produces one
    /// complete raw BM25 query score per matching document, and the wrapper
    /// applies the persisted field calibration exactly once.
    BayesianScore {
        source: Box<OperatorTree>,
        field: Option<String>,
    },
    /// Bayesian text retrieval combined with a document authority or recency
    /// prior stored in another field.
    BayesianMatchWithPrior {
        field: String,
        query: String,
        prior_field: String,
        mode: ExternalPriorMode,
    },

    /// `IntersectOperator([...])`.
    Intersect(Vec<OperatorTree>),
    /// `UnionOperator([...])`.
    Union(Vec<OperatorTree>),
    /// `ComplementOperator(operand)`.
    Complement(Box<OperatorTree>),
    /// `ComposedOperator([...])`.
    Composed(Vec<OperatorTree>),

    /// `VectorSimilarityOperator(query_vector, threshold, field)`.
    VectorSimilarity {
        query_vector: Vec<f32>,
        threshold: f32,
        field: String,
    },
    /// `KNNOperator(query_vector, k, field)`.
    KNN {
        query_vector: Vec<f32>,
        k: usize,
        field: String,
    },
    /// Pool-calibrated KNN retrieval exposed by
    /// `calibrated_vector_match`.
    CalibratedVectorMatch {
        query_vector: Vec<f32>,
        k: usize,
        field: String,
        threshold: Option<f64>,
    },
    /// `CosineProbabilityOperator(source)` -- wraps a KNN child with a
    /// calibrated probability projection.
    CosineProbability(Box<OperatorTree>),

    /// Exact signed-evidence Bayesian fusion. `base_rate = None` derives one
    /// prior from signal metadata and otherwise falls back to the neutral 0.5.
    BayesianEvidenceFusion {
        signals: Vec<OperatorTree>,
        base_rate: Option<f64>,
    },
    /// Robust positive-evidence retrieval pool with optional weights and logit
    /// normalization. This variant makes no calibration theorem claim.
    RobustPositiveEvidencePool {
        signals: Vec<OperatorTree>,
        alpha: f64,
        gating: GatingSpec,
        weights: Option<Vec<f64>>,
        logit_min: Option<Vec<f64>>,
        logit_max: Option<Vec<f64>>,
        /// Derive weights from the score spread of this invocation.
        adaptive_weights: bool,
    },
    /// `ProbBoolFusionOperator(signals, mode)`.
    ProbBoolFusion {
        signals: Vec<OperatorTree>,
        mode: ProbBoolMode,
    },
    /// `ProbNotOperator(signal, default_prob)`.
    ProbNot {
        signal: Box<OperatorTree>,
        default_prob: f64,
    },
    /// `AttentionFusionOperator(signals, attention, query_features)`.
    AttentionFusion {
        signals: Vec<OperatorTree>,
        attention: AttentionRef,
        query_features: Vec<f64>,
    },
    /// `LearnedFusionOperator(signals, learned)`.
    LearnedFusion {
        signals: Vec<OperatorTree>,
        learned: LearnedFusionRef,
    },
    /// `SparseThresholdOperator(source, threshold)`.
    SparseThreshold {
        source: Box<OperatorTree>,
        threshold: f64,
    },

    /// `TraverseOperator(start, graph, label, max_hops, vertex_predicate)`.
    Traverse {
        start_vertex: u64,
        graph: String,
        label: Option<String>,
        max_hops: usize,
        vertex_predicate: Option<VertexPredicate>,
    },
    /// One-hop graph neighborhood without including the start vertex.
    /// This is separate from `Traverse(max_hops=1)` because SQL
    /// `graph_neighbors` also carries an explicit edge direction and has
    /// different start-vertex semantics.
    GraphNeighbors {
        vertex: u64,
        graph: String,
        label: Option<String>,
        direction: Direction,
    },
    /// Emit graph edges as posting entries keyed by edge id. The payload
    /// score carries the optional numeric edge weight.
    GraphEdges {
        graph: String,
        label: Option<String>,
    },
    /// `PatternMatchOperator(pattern, graph)`.
    PatternMatch {
        pattern: GraphPatternIR,
        graph: String,
    },
    /// `RegularPathQueryOperator(expr, start, graph)`.
    RegularPathQuery {
        rpq_source: String,
        start_vertex: u64,
        graph: String,
    },
    /// `GraphJoinOperator(left, right, label, graph)`.
    GraphJoin {
        left: Box<OperatorTree>,
        right: Box<OperatorTree>,
        label: Option<String>,
        graph: String,
    },

    /// `IndexScanOperator(index, field, predicate)` -- selected by the
    /// optimizer when a covering index is cheaper than a full scan.
    IndexScan {
        index_name: String,
        field: String,
        predicate: Predicate,
    },

    /// `AggregateOperator(source, field, monoid)`.
    Aggregate {
        source: Option<Box<OperatorTree>>,
        field: String,
        monoid: Arc<dyn AggregationMonoid>,
    },
    /// `GroupByOperator(source, group_field, agg_field, monoid)`.
    GroupBy {
        source: Box<OperatorTree>,
        group_field: String,
        agg_field: String,
        monoid: Arc<dyn AggregationMonoid>,
    },

    // -----------------------------------------------------------------
    // Cross-paradigm operators (mirrors `_estimate_cross_paradigm`).
    // -----------------------------------------------------------------
    /// `MultiStageOperator(stages=[(child, cutoff), ...])`. The cutoff
    /// determines the cardinality at the final stage.
    MultiStage { stages: Vec<MultiStageEntry> },
    /// `MultiFieldSearchOperator(fields, queries, weights)`.
    MultiFieldSearch {
        fields: Vec<String>,
        queries: Vec<String>,
        weights: Option<Vec<f64>>,
    },
    /// `HybridTextVectorOperator(term_op, vector_op, alpha)`.
    HybridTextVector {
        term_op: Box<OperatorTree>,
        vector_op: Box<OperatorTree>,
        alpha: f64,
    },
    /// `SemanticFilterOperator(source, vector_op)`.
    SemanticFilter {
        source: Box<OperatorTree>,
        vector_op: Box<OperatorTree>,
    },
    /// `VectorExclusionOperator(positive, negative_op)`.
    VectorExclusion {
        positive: Box<OperatorTree>,
        negative: Box<OperatorTree>,
    },
    /// `FacetVectorOperator(vector_op, facet_field)`.
    FacetVector {
        vector_op: Box<OperatorTree>,
        facet_field: String,
    },
    /// `VertexAggregationOperator(source, monoid)` -- single-row result.
    VertexAggregation {
        source: Box<OperatorTree>,
        monoid: Arc<dyn AggregationMonoid>,
    },
    /// A bounded regular-path walk filtered by its accumulated edge weight.
    /// `predicate_selectivity` is a planner estimate only; the physical
    /// predicate itself is always preserved in `predicate`.
    WeightedPathQuery {
        rpq_source: String,
        start_vertex: u64,
        graph: String,
        weight_property: String,
        default_edge_weight: f64,
        max_hops: usize,
        predicate: PathWeightPredicate,
        predicate_selectivity: f64,
        score: f64,
    },
    /// `MessagePassingOperator(source, ...)` -- pass-through cardinality.
    MessagePassing { source: Box<OperatorTree> },
    /// `GraphEmbeddingOperator(source, ...)` -- pass-through cardinality.
    GraphEmbedding { source: Box<OperatorTree> },
    /// `PageRankOperator(graph)` -- one score per vertex.
    PageRank { graph: String },
    /// `HITSOperator(graph)` -- one score per vertex.
    HITS { graph: String },
    /// `BetweennessCentralityOperator(graph)` -- one score per vertex.
    BetweennessCentrality { graph: String },
    /// `TextSimilarityJoinOperator(left, right, threshold)`.
    TextSimilarityJoin {
        left: Box<OperatorTree>,
        right: Box<OperatorTree>,
        threshold: f64,
    },
    /// `VectorSimilarityJoinOperator(left, right, threshold)`.
    VectorSimilarityJoin {
        left: Box<OperatorTree>,
        right: Box<OperatorTree>,
        threshold: f64,
    },
    /// `HybridJoinOperator(left, right)`.
    HybridJoin {
        left: Box<OperatorTree>,
        right: Box<OperatorTree>,
    },
    /// `CrossParadigmJoinOperator(left, right)`. Distinct from
    /// [`OperatorTree::GraphJoin`]: it joins arbitrary operands via a
    /// graph traversal step but does not carry an edge label.
    CrossParadigmJoin {
        left: Box<OperatorTree>,
        right: Box<OperatorTree>,
    },
    /// `TemporalTraverseOperator(start, graph, label, hops, filter)`.
    TemporalTraverse {
        start_vertex: u64,
        graph: String,
        label: Option<String>,
        max_hops: usize,
        temporal_filter: Option<TemporalFilterIR>,
    },
    /// `TemporalPatternMatchOperator(pattern, graph, filter)`.
    TemporalPatternMatch {
        pattern: GraphPatternIR,
        graph: String,
        temporal_filter: Option<TemporalFilterIR>,
    },
    /// `ProgressiveFusionOperator(stages=[(signal, k), ...], alpha, gating)`.
    /// The final stage `k` determines the result cardinality.
    ProgressiveFusion {
        stages: Vec<ProgressiveFusionEntry>,
        alpha: f64,
        gating: GatingSpec,
    },
    /// `DeepFusionOperator(layers, alpha, gating)`.
    DeepFusion {
        layers: Vec<DeepFusionLayer>,
        alpha: f64,
        gating: GatingSpec,
    },

    /// Execute a registered deep model and emit its document scores.
    DeepPredict { model: String },

    /// Catch-all for opaque operators the optimizer should not rewrite.
    Opaque {
        kind: String,
        children: Vec<OperatorTree>,
        meta: BTreeMap<String, Value>,
    },
}

/// A single entry in a [`OperatorTree::MultiStage`] cascade. Mirrors
/// `MultiStageOperator.stages` -- `(child, cutoff)` -- where the
/// cutoff is either a fixed top-K or a fractional ratio.
#[derive(Clone)]
pub struct MultiStageEntry {
    pub child: OperatorTree,
    pub cutoff: MultiStageCutoff,
}

/// Mirrors `multi_stage.Cutoff`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MultiStageCutoff {
    /// Top-K results -- final cardinality is `k`.
    TopK(usize),
    /// Fractional cutoff -- final cardinality is `n * ratio`.
    Ratio(f64),
}

/// One stage of a [`OperatorTree::ProgressiveFusion`].
#[derive(Clone)]
pub struct ProgressiveFusionEntry {
    pub signal: OperatorTree,
    pub k: usize,
}

/// Layer in a [`OperatorTree::DeepFusion`] pipeline. Mirrors
/// `uqa.operators.deep_fusion.{ConvLayer, FlattenLayer, PoolLayer,
/// PropagateLayer, SignalLayer, DenseLayer, SoftmaxLayer,
/// BatchNormLayer, DropoutLayer}`.
#[derive(Clone)]
pub enum DeepFusionLayer {
    Signal {
        signals: Vec<OperatorTree>,
    },
    Propagate {
        edge_label: Option<String>,
        aggregation: DeepFusionAggregation,
        direction: Direction,
    },
    Conv {
        edge_label: Option<String>,
        /// Self weight followed by one weight per neighbor hop.
        hop_weights: Vec<f64>,
        direction: Direction,
    },
    Pool {
        edge_label: Option<String>,
        pool_size: usize,
        method: DeepFusionPoolMethod,
        direction: Direction,
    },
    Flatten,
    Dense {
        /// `output_channels x input_channels`, row-major.
        weights: Vec<f64>,
        bias: Vec<f64>,
        output_channels: usize,
        input_channels: usize,
    },
    Softmax,
    BatchNorm {
        epsilon: f64,
    },
    Dropout {
        probability: f64,
    },
}

/// Tree-local view of a temporal filter. Mirrors
/// `uqa.graph.temporal_filter.TemporalFilter`. The filter accepts
/// either an exact timestamp or a `[low, high]` time range; both can
/// be present (the canonical UQA implementation's class follows the same shape).
#[derive(Clone, Debug, Default)]
pub struct TemporalFilterIR {
    pub timestamp: Option<f64>,
    pub time_range: Option<(f64, f64)>,
}

impl OperatorTree {
    /// Visit this node and every descendant in pre-order.
    ///
    /// The match is intentionally exhaustive so adding a child-bearing IR
    /// variant cannot silently create a traversal boundary in planners or
    /// engine catalog analysis.
    #[allow(clippy::too_many_lines)]
    pub fn visit(&self, visitor: &mut impl FnMut(&OperatorTree)) {
        visitor(self);
        match self {
            OperatorTree::Filter {
                source: Some(source),
                ..
            }
            | OperatorTree::Facet {
                source: Some(source),
                ..
            }
            | OperatorTree::Score { source, .. }
            | OperatorTree::BayesianScore { source, .. }
            | OperatorTree::Complement(source)
            | OperatorTree::CosineProbability(source)
            | OperatorTree::ProbNot { signal: source, .. }
            | OperatorTree::SparseThreshold { source, .. }
            | OperatorTree::VertexAggregation { source, .. }
            | OperatorTree::MessagePassing { source }
            | OperatorTree::GraphEmbedding { source }
            | OperatorTree::GroupBy { source, .. }
            | OperatorTree::Aggregate {
                source: Some(source),
                ..
            } => source.visit(visitor),
            OperatorTree::Intersect(children)
            | OperatorTree::Union(children)
            | OperatorTree::Composed(children)
            | OperatorTree::Opaque { children, .. }
            | OperatorTree::BayesianEvidenceFusion {
                signals: children, ..
            }
            | OperatorTree::RobustPositiveEvidencePool {
                signals: children, ..
            }
            | OperatorTree::ProbBoolFusion {
                signals: children, ..
            }
            | OperatorTree::AttentionFusion {
                signals: children, ..
            }
            | OperatorTree::LearnedFusion {
                signals: children, ..
            } => visit_operator_slice(children, visitor),
            OperatorTree::GraphJoin { left, right, .. }
            | OperatorTree::TextSimilarityJoin { left, right, .. }
            | OperatorTree::VectorSimilarityJoin { left, right, .. }
            | OperatorTree::HybridJoin { left, right }
            | OperatorTree::CrossParadigmJoin { left, right }
            | OperatorTree::HybridTextVector {
                term_op: left,
                vector_op: right,
                ..
            }
            | OperatorTree::SemanticFilter {
                source: left,
                vector_op: right,
            }
            | OperatorTree::VectorExclusion {
                positive: left,
                negative: right,
            } => {
                left.visit(visitor);
                right.visit(visitor);
            }
            OperatorTree::FacetVector { vector_op, .. } => vector_op.visit(visitor),
            OperatorTree::MultiStage { stages } => {
                for stage in stages {
                    stage.child.visit(visitor);
                }
            }
            OperatorTree::ProgressiveFusion { stages, .. } => {
                for stage in stages {
                    stage.signal.visit(visitor);
                }
            }
            OperatorTree::DeepFusion { layers, .. } => {
                for layer in layers {
                    if let DeepFusionLayer::Signal { signals } = layer {
                        for signal in signals {
                            signal.visit(visitor);
                        }
                    }
                }
            }
            OperatorTree::Empty
            | OperatorTree::Term { .. }
            | OperatorTree::BayesianMatchWithPrior { .. }
            | OperatorTree::Filter { source: None, .. }
            | OperatorTree::Facet { source: None, .. }
            | OperatorTree::VectorSimilarity { .. }
            | OperatorTree::KNN { .. }
            | OperatorTree::CalibratedVectorMatch { .. }
            | OperatorTree::Traverse { .. }
            | OperatorTree::GraphNeighbors { .. }
            | OperatorTree::GraphEdges { .. }
            | OperatorTree::PatternMatch { .. }
            | OperatorTree::RegularPathQuery { .. }
            | OperatorTree::IndexScan { .. }
            | OperatorTree::Aggregate { source: None, .. }
            | OperatorTree::MultiFieldSearch { .. }
            | OperatorTree::WeightedPathQuery { .. }
            | OperatorTree::PageRank { .. }
            | OperatorTree::HITS { .. }
            | OperatorTree::BetweennessCentrality { .. }
            | OperatorTree::TemporalTraverse { .. }
            | OperatorTree::TemporalPatternMatch { .. }
            | OperatorTree::DeepPredict { .. } => {}
        }
    }

    /// `True` when the operator is structurally empty. The explicit empty
    /// node and zero-operand boolean/composition nodes all execute to an
    /// empty posting list, so the optimizer must give them the same meaning.
    pub fn is_empty(&self) -> bool {
        match self {
            OperatorTree::Empty => true,
            OperatorTree::Intersect(v) | OperatorTree::Union(v) | OperatorTree::Composed(v) => {
                v.is_empty()
            }
            _ => false,
        }
    }
}

fn visit_operator_slice(children: &[OperatorTree], visitor: &mut impl FnMut(&OperatorTree)) {
    for child in children {
        child.visit(visitor);
    }
}
