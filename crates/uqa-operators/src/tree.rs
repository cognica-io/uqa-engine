//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Operator tree IR for the planner.
//!
//! Mirrors `uqa/operators/base.py` + the concrete operator subclass
//! taxonomy. Python's [`QueryOptimizer`] uses `isinstance` to walk an
//! operator tree and rewrite it; the Rust port lifts every concrete
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
    /// Raw signal score scales the fused logit.
    Pass,
    /// Sigmoid gating with the named feature.
    Sigmoid { feature: String },
    /// `ReLU` gate.
    ReLU,
}

/// Concrete operator tree mirroring `uqa/operators` Python class hierarchy.
#[derive(Clone)]
pub enum OperatorTree {
    /// Empty leaf (no input). The optimizer treats `Intersect([])` and
    /// `Union([])` as empty when checking absorption rules.
    Empty,

    /// `TermOperator(query_string, field)` -- text retrieval primitive.
    Term {
        query: String,
        field: Option<String>,
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

    /// `LogOddsFusionOperator(signals, alpha, gating)`.
    LogOddsFusion {
        signals: Vec<OperatorTree>,
        alpha: f64,
        gating: GatingSpec,
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

    /// Catch-all for opaque operators the optimizer should not rewrite.
    Opaque {
        kind: String,
        children: Vec<OperatorTree>,
        meta: BTreeMap<String, Value>,
    },
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

    /// Pointer-style identity for the absorption rule. The Python
    /// optimizer compares operands by `is`; the Rust port uses
    /// pointer-stable structural fingerprints.
    pub fn fingerprint(&self) -> usize {
        // Use the address of the heap-stored variant payload as the
        // identity. Cheap clones that share the same Box / Arc
        // payloads will collide; structurally-equal but separately
        // allocated trees won't, matching Python's `is` semantics.
        std::ptr::from_ref::<OperatorTree>(self) as usize
    }
}
