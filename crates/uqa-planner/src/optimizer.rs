//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Plan-native algebraic and cross-paradigm rewrites.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::Value;
use uqa_sql::ast::BinaryOp;
use uqa_sql::{ScalarExpr, ScalarFrameBound};

use crate::unified_plan::{
    AccessPathPlan, AggregateClassifier, CommandPlan, ComputePlan, ExpressionPlan, ProjectionPlan,
    QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan,
};
use crate::{JoinGraphError, JoinOrderOptimizer, JoinPredicate, JoinRelation, RelationStats};

#[cfg(test)]
use crate::JoinExecutionStrategy;

mod access_path;
mod api;
mod cte_projection;
mod derived_projection;
mod derived_pullup;
mod function_source;
mod implicit_fusion;
mod join_reorder;
mod output_demand;
mod owned;
mod scalar;
mod source_constants;
mod source_demand;
mod subqueries;
mod union_projection;

pub use access_path::contains_retrieval;
pub(crate) use api::ConstantStorageFactory;
pub use api::{
    optimize, optimize_owned_scalar_expression, optimize_owned_with_aggregates_and_statistics,
    optimize_scalar_expression, optimize_with_aggregates, optimize_with_aggregates_and_statistics,
    optimize_with_statistics, ConstantEvaluator, OptimizerConfig, OptimizerError, OptimizerResult,
    SourceStatistics,
};
pub(crate) use derived_projection::expand_local_stars;
pub(crate) use derived_projection::owned::expand_stars as expand_local_stars_owned;
pub use derived_projection::{
    prune_outputs as prune_query_outputs, prune_owned_outputs as prune_owned_query_outputs,
    OutputPruningContext,
};
pub use implicit_fusion::query_contains_implicit_hybrid_fusion;

use access_path::choose_access_path;
use implicit_fusion::source_allows_unqualified_signals;
pub(crate) use scalar::constants::{retain_computed_integer, retain_computed_integer_owned};
pub(crate) use scalar::optimize_qualification;
pub(crate) use scalar::optimize_qualification_owned;
use scalar::optimize_scalar_slot;

#[cfg(test)]
mod tests;
