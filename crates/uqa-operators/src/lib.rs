//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Algebraic operators over posting lists.
//!
//! Operators form a monoid under composition (Theorem 3.2.3, Paper 1):
//! every concrete operator's `execute` returns a `PostingList`, and
//! [`ComposedOperator`] is associative with the empty operator as
//! identity.

pub mod aggregation;
pub mod base;
pub mod boolean;
pub mod deep_fusion;
pub mod fusion_wrappers;
pub mod hierarchical;
pub mod hybrid;
pub mod multi_stage;
pub mod primitive;
pub mod progressive_fusion;
pub mod sparse;
pub mod tree;
pub mod vector;

pub use aggregation::{
    AggState, AggregateOperator, AggregationMonoid, AvgMonoid, AvgState, CountMonoid,
    GroupByOperator, MaxMonoid, MinMonoid, QuantileMonoid, SumMonoid,
};
pub use base::{
    ComposedOperator, Direction as DeepGraphDirection, ExecutionContext, GraphNeighborLookup,
    Operator,
};
pub use boolean::{ComplementOperator, IntersectOperator, UnionOperator};
pub use deep_fusion::{
    AggregationKind as DeepAggKind, DeepFusionOperator, Gating, GlobalPoolMethod, Layer,
    PoolMethod as DeepPoolMethod,
};
pub use fusion_wrappers::{
    AttentionFuser, AttentionFusionOperator, CalibratedVectorOperator, LearnedFusionOperator,
    MultiFieldSearchOperator, WeightSource,
};
pub use hierarchical::{
    parse_path, AggregationKind as PathAggKind, PathAggregateOperator, PathExpr,
    PathFilterOperator, PathProjectOperator, PathSegment, UnifiedFilterOperator,
};
pub use hybrid::{HybridTextVectorOperator, LogOddsFusionOperator, SemanticFilterOperator};
pub use multi_stage::{Cutoff, MultiStageOperator};
pub use primitive::{
    FacetOperator, FilterOperator, ScoreOperator, SpatialWithinOperator, TermOperator,
};
pub use progressive_fusion::ProgressiveFusionOperator;
pub use sparse::SparseThresholdOperator;
pub use tree::{
    AttentionFuserDyn, AttentionRef, DeepFusionLayer, EdgeConstraint, EdgePatternIR, GatingSpec,
    GraphPatternIR, LearnedFuserDyn, LearnedFusionRef, MultiStageCutoff, MultiStageEntry,
    OperatorTree, ProbBoolMode, ProgressiveFusionEntry, ScorerRef, TemporalFilterIR,
    VertexConstraint, VertexPatternIR, VertexPredicate,
};
pub use vector::{CosineProbabilityOperator, KNNOperator, VectorSimilarityOperator};
