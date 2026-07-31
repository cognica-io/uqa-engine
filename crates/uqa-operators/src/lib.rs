//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document-support, payload, scoring, and fusion operators over posting lists.
//!
//! Operators form a monoid under composition (Theorem 3.2.3, Paper 1):
//! every concrete operator's `execute` returns a `PostingList`, and
//! [`ComposedOperator`] is associative with the empty operator as
//! identity.

pub mod aggregation;
pub mod base;
pub mod boolean;
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
#[allow(deprecated)]
pub use fusion_wrappers::{
    fit_pool_calibration, AttentionFuser, AttentionFusionOperator, CalibratedVectorOperator,
    LearnedFusionOperator, MultiFieldSearchOperator, QueryPoolVectorScoreOperator,
    RelevantSampleSplit,
};
pub use hierarchical::{
    eval_path, parse_path, project_paths, unnest_array, AggregationKind as PathAggKind,
    PathAggregateOperator, PathFilterOperator, PathProjectOperator, UnifiedFilterOperator,
};
pub use hybrid::{
    AdaptivePositiveEvidencePoolOperator, BayesianEvidenceFusionOperator, FacetVectorOperator,
    HybridTextVectorOperator, IndexScanOperator, ProbBoolFusionOperator,
    ProbBoolMode as HybridProbBoolMode, ProbNotOperator, RobustPositiveEvidencePoolOperator,
    SemanticFilterOperator, VectorExclusionOperator,
};
pub use multi_stage::{Cutoff, MultiStageOperator};
pub use primitive::{
    FacetOperator, FilterOperator, ScoreOperator, SpatialWithinOperator, TermOperator,
};
pub use progressive_fusion::ProgressiveFusionOperator;
pub use sparse::SparseThresholdOperator;
pub use tree::{
    AttentionFuserDyn, AttentionRef, DeepFusionAggregation, DeepFusionLayer, DeepFusionPoolMethod,
    EdgeConstraint, EdgePatternIR, ExternalPriorMode, GatingSpec, GraphPatternIR, LearnedFuserDyn,
    LearnedFusionRef, MultiStageCutoff, MultiStageEntry, OperatorTree, PathWeightPredicate,
    ProbBoolMode, ProgressiveFusionEntry, ScorerRef, TemporalFilterIR, TextScoringMode,
    VertexConstraint, VertexPatternIR, VertexPredicate,
};
pub use uqa_core::{PathExpr, PathSegment};
pub use vector::{CosineProbabilityOperator, KNNOperator, VectorSimilarityOperator};
