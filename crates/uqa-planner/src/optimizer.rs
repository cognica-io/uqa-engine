//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Plan-native algebraic and cross-paradigm rewrites.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::Value;
use uqa_execution::{ScalarExpr, ScalarFrameBound};
use uqa_sql::ast::BinaryOp;

use crate::unified_plan::{
    AccessPathPlan, AggregateClassifier, AssignmentPlan, CommandPlan, ComputePlan,
    ConflictActionPlan, ExpressionPlan, JoinExecutionStrategy, MergeWhenPlan, ProjectionPlan,
    QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan,
};
use crate::{
    JoinAlgorithm, JoinGraphError, JoinGraphResult, JoinOrderOptimizer, JoinOrderTree,
    JoinPredicate, JoinRelation, RelationStats,
};

mod access_path;
mod api;
mod command;
mod join_reorder;
mod scalar;
mod traversal;

pub use access_path::contains_retrieval;
pub use api::{
    optimize, optimize_with_aggregates, optimize_with_aggregates_and_statistics,
    optimize_with_statistics, OptimizerConfig, SourceStatistics,
};

use access_path::{choose_access_path, prioritize_access_predicates};
use command::optimize_command;
use join_reorder::reorder_unified_plan_joins;
use scalar::{optimize_assignments, optimize_projections, optimize_scalar_slot};
use traversal::{optimize_query, optimize_source, optimize_unified_plan};

#[cfg(test)]
mod tests;
