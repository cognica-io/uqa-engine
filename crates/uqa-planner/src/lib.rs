//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query optimizer: cost estimation, cardinality, join enumeration.
//!
//! Layout follows the canonical UQA behavior (`uqa/planner/*`):
//!
//! * [`cost_model`] -- per-operator cost model. Estimates a unitless
//!   cost for scans, filters, projections, sorts, hash aggregates,
//!   window operators, and join algorithms. Used by the join
//!   enumerator to pick a winning plan.
//! * [`cardinality`] -- per-relation [`RelationStats`] +
//!   [`CardinalityEstimator`] that turn predicate selectivities into
//!   row-count estimates. Equality, range, and `LIKE` selectivities
//!   are all on the same scale (`0..=1`).
//! * [`join_graph`] -- [`JoinEdge`] / [`JoinGraph`] -- the dataflow
//!   graph the enumerator walks.
//! * [`join_enumerator`] -- DPccp (Moerkotte/Neumann 2006). Bitmask
//!   `u64` for relation subsets, `HashMap` for the DP cache.
//! * [`optimizer`] -- algebraic rewrites: filter pushdown, vector
//!   threshold merging, facet additivity, Boolean simplification.
//! * [`parallel`] -- rayon-backed parallel-aware split + recombine.
//! * [`executor`] -- the planner-to-physical-operator bridge.

#![allow(
    clippy::enum_glob_use,
    clippy::doc_markdown,
    clippy::map_unwrap_or,
    clippy::derivable_impls,
    clippy::match_same_arms,
    clippy::module_name_repetitions,
    clippy::missing_panics_doc,
    clippy::panic_in_result_fn,
    clippy::needless_for_each,
    clippy::manual_assert,
    clippy::option_if_let_else,
    clippy::similar_names,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::unnecessary_wraps,
    dead_code,
    unused_imports,
    clippy::struct_excessive_bools,
    clippy::only_used_in_recursion,
    clippy::question_mark,
    clippy::unused_self,
    clippy::single_match_else
)]

pub mod cardinality;
pub mod cost_model;
pub mod executor;
pub mod join_enumerator;
pub mod join_graph;
pub mod join_order;
pub mod optimizer;
pub mod parallel;
pub mod query_optimizer;
pub mod unified_plan;

pub use cardinality::{
    column_entropy, entropy_cardinality_lower_bound, mutual_information_estimate,
    CardinalityEstimator, ColumnStats, EdgeSample, GraphStats, GraphStoreSampler, RelationStats,
    Selectivity, GRAPH_AVG_DEGREE_DEFAULT, JACCARD_JOIN_SELECTIVITY, VECTOR_JOIN_SELECTIVITY,
};
pub use cost_model::{CostEstimator, OperatorCost, OperatorKind};
pub use executor::{ExecutionStats, OperatorOutput, OperatorTreeDriver, PlanExecutor};
pub use join_enumerator::{enumerate_dpccp, JoinPlan};
pub use join_graph::{JoinEdge, JoinGraph};
pub use join_order::{
    JoinAlgorithm, JoinCondition, JoinOrderOptimizer, JoinOrderResult, JoinOrderTree,
    JoinPredicate, JoinRelation, INDEX_JOIN_THRESHOLD,
};
pub use optimizer::{optimize, OptimizerConfig};
pub use parallel::{
    run_parallel, ParallelExecutor, DEFAULT_PARALLEL_WORKERS, MIN_PARALLEL_BRANCHES,
};
pub use query_optimizer::{
    IndexScanCandidate, OptimizerConfig as TreeOptimizerConfig, QueryOptimizer,
};
pub use unified_plan::{
    AggregateClassifier, CommandPlan, ComputePlan, CtePlan, ExpressionPlan, OrderPlan,
    ProjectionPlan, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan, UnifiedExecutionStats,
    UnifiedPlan, UnifiedPlanDriver, UnifiedPlanExecutor,
};
