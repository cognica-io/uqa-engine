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

/// Function pointer for a vertex constraint (used by pattern match).
pub type VertexConstraint = Arc<dyn Fn(&uqa_core::Vertex) -> bool + Send + Sync>;

/// Function pointer for an edge constraint (used by pattern match).
pub type EdgeConstraint = Arc<dyn Fn(&uqa_core::Edge) -> bool + Send + Sync>;

pub trait AttentionFuserDyn: Send + Sync {
    fn fuse(&self, probs: &[f64], query_features: &[f64]) -> f64;
}

pub trait LearnedFuserDyn: Send + Sync {
    fn fuse(&self, probs: &[f64]) -> f64;
}

impl AttentionFuserDyn for uqa_fusion::AttentionFusion {
    fn fuse(&self, probs: &[f64], query_features: &[f64]) -> f64 {
        uqa_fusion::AttentionFusion::fuse(self, probs, query_features)
    }
}

impl AttentionFuserDyn for uqa_fusion::MultiHeadAttentionFusion {
    fn fuse(&self, probs: &[f64], query_features: &[f64]) -> f64 {
        uqa_fusion::MultiHeadAttentionFusion::fuse(self, probs, query_features)
    }
}

impl LearnedFuserDyn for uqa_fusion::LearnedFusion {
    fn fuse(&self, probs: &[f64]) -> f64 {
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

/// Text scoring algorithm used by [`OperatorTree::Term`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextScoringMode {
    BM25,
    BayesianBM25,
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
    /// `CosineProbabilityOperator(source)` -- wraps a KNN child with a
    /// calibrated probability projection.
    CosineProbability(Box<OperatorTree>),

    /// `LogOddsFusionOperator` with optional Lucene weights and logit bounds.
    LogOddsFusion {
        signals: Vec<OperatorTree>,
        alpha: f64,
        gating: GatingSpec,
        weights: Option<Vec<f64>>,
        logit_min: Option<Vec<f64>>,
        logit_max: Option<Vec<f64>>,
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
    /// `MultiFieldSearchOperator(fields, query, weights)`.
    MultiFieldSearch {
        fields: Vec<String>,
        query: String,
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
    /// `WeightedPathQueryOperator(rpq, start, graph, predicate)`.
    WeightedPathQuery {
        rpq_source: String,
        start_vertex: u64,
        graph: String,
        predicate_selectivity: f64,
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
    /// `ProgressiveFusionOperator(stages=[(signal, k), ...])`. The
    /// final stage `k` determines the result cardinality.
    ProgressiveFusion { stages: Vec<ProgressiveFusionEntry> },
    /// `DeepFusionOperator(layers, alpha, gating)`.
    DeepFusion {
        layers: Vec<DeepFusionLayer>,
        alpha: f64,
        gating: GatingSpec,
    },

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
    Signal { signals: Vec<OperatorTree> },
    Propagate { edge_label: Option<String> },
    Conv,
    Pool { pool_size: f64 },
    Flatten,
    Dense,
    Softmax,
    BatchNorm,
    Dropout,
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
    /// `True` when the operator is structurally empty: an
    /// `Intersect([])` or `Union([])`. Mirrors
    /// `QueryOptimizer._is_empty_operator`.
    pub fn is_empty(&self) -> bool {
        match self {
            OperatorTree::Empty => true,
            OperatorTree::Intersect(v) | OperatorTree::Union(v) => v.is_empty(),
            _ => false,
        }
    }

    /// Pointer-style identity for the absorption rule. The UQA
    /// optimizer compares operands by `is`; the UQA-RS implementation uses
    /// pointer-stable structural fingerprints.
    pub fn fingerprint(&self) -> usize {
        // Use the address of the heap-stored variant payload as the
        // identity. Cheap clones that share the same Box / Arc
        // payloads will collide; structurally-equal but separately
        // allocated trees won't, matching the canonical UQA implementation's `is` semantics.
        std::ptr::from_ref::<OperatorTree>(self) as usize
    }
}
