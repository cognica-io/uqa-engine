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
    AccessPathPlan, AggregateClassifier, AssignmentPlan, CommandPlan, ComputePlan,
    ConflictActionPlan, ExpressionPlan, JoinExecutionStrategy, MergeWhenPlan, ProjectionPlan,
    QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan,
};
use crate::{
    JoinGraphError, JoinGraphResult, JoinOrderOptimizer, JoinPredicate, JoinRelation, RelationStats,
};

mod access_path;
mod api;
mod command;
mod cte_projection;
mod derived_projection;
mod derived_pullup;
mod function_source;
mod implicit_fusion;
mod join_reorder;
mod output_demand;
mod scalar;
mod source_constants;
mod source_demand;
mod subqueries;
mod traversal;
mod union_projection;

pub use access_path::contains_retrieval;
pub(crate) use api::ConstantStorageFactory;
pub use api::{
    optimize, optimize_scalar_expression, optimize_with_aggregates,
    optimize_with_aggregates_and_statistics, optimize_with_statistics, ConstantEvaluator,
    OptimizerConfig, OptimizerError, OptimizerResult, SourceStatistics,
};
pub(crate) use derived_projection::expand_local_stars;
pub use derived_projection::{prune_outputs as prune_query_outputs, OutputPruningContext};
pub use implicit_fusion::query_contains_implicit_hybrid_fusion;

use access_path::{choose_access_path, prioritize_access_predicates};
use command::optimize_command;
use implicit_fusion::{rewrite_implicit_hybrid_fusion, source_allows_unqualified_signals};
use join_reorder::reorder_unified_plan_joins;
pub(crate) use scalar::constants::retain_computed_integer;
pub(crate) use scalar::optimize_qualification;
use scalar::{optimize_assignments, optimize_projections, optimize_scalar_slot};
use traversal::{optimize_query, optimize_source, optimize_unified_plan};

#[cfg(test)]
mod tests;
