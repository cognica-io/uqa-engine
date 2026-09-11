//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public Engine retrieval entry points and statement-boundary composition.
//!
//! SQL binds predicates and retrieval calls into its runtime-independent retrieval algebra.
//! Execution evaluates scalar arguments and instantiates physical models. The public
//! [`lower_where`] re-export and [`EngineDriver`] preserve existing Rust entry points.
//! The driver enters the active statement and transaction before physical execution.

use uqa_execution::operator_tree::{OperatorOutput, OperatorTreeDriver};
use uqa_execution::parallel::ParallelExecutor;
use uqa_execution::ScalarExpr;
use uqa_operators::OperatorTree;
use uqa_planner::retrieval_planning::{
    estimate_cross_relation_operator_join, estimate_operator_tree_access, query_optimizer,
};
use uqa_sql::ast::OperatorJoinRelations;
use uqa_sql::SQLParam;

use crate::{Engine, ScoredEntry};
use uqa_sql::SQLError;

pub use uqa_execution::operator_tree::binding::lower_where;

type DriverResult<T> = Result<T, SQLError>;

fn operator_execution_error(operator: &str, error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("execute {operator}: {error}"))
}

/// Bind a physical retrieval driver to the Engine statement and transaction boundary.
pub struct EngineDriver<'a> {
    pub engine: &'a Engine,
    pub table: &'a str,
    pub params: &'a [SQLParam],
    pub parallel: ParallelExecutor,
}

impl<'a> EngineDriver<'a> {
    #[must_use]
    pub fn new(engine: &'a Engine, table: &'a str, params: &'a [SQLParam]) -> Self {
        Self {
            engine,
            table,
            params,
            parallel: ParallelExecutor::default(),
        }
    }
    #[must_use]
    pub fn with_parallel(mut self, parallel: ParallelExecutor) -> Self {
        self.parallel = parallel;
        self
    }
}

impl OperatorTreeDriver for EngineDriver<'_> {
    type Error = SQLError;
    fn execute_node(&self, tree: &OperatorTree) -> DriverResult<OperatorOutput> {
        execution::execute_public_physical_node(self, tree)
    }
}

/// Lower a WHERE expression and run [`uqa_planner::query_optimizer::QueryOptimizer`] over the
/// resulting tree without executing it. Useful for tests and
/// `EXPLAIN`-style diagnostics that want to inspect the rewritten
/// shape before any posting list is materialised.
pub fn optimised_tree_for(
    engine: &Engine,
    table: &str,
    where_expr: &ScalarExpr,
    params: &[SQLParam],
) -> DriverResult<Option<OperatorTree>> {
    let Some(tree) = engine.retrieval_binding().lower_where(where_expr, params)? else {
        return Ok(None);
    };
    Ok(Some(query_optimizer(engine, table, &tree)?.optimize(tree)))
}

/// Cost a relation-local SQL predicate through the same lowering and
/// optimizer configuration used by execution.
pub(crate) fn estimate_local_access(
    engine: &Engine,
    table: &str,
    where_expr: &ScalarExpr,
    params: &[SQLParam],
) -> DriverResult<Option<uqa_planner::LocalAccessEstimate>> {
    let Some(tree) = engine.retrieval_binding().lower_where(where_expr, params)? else {
        return Ok(None);
    };
    estimate_operator_tree_access(engine, table, tree, true).map(Some)
}

pub(crate) use uqa_execution::query::table_sources::retrieval::DirectVectorRetrieval;

/// Describe a complete predicate that owns one bounded vector candidate pool.
/// A hierarchy scan applies that pool and any query-local calibration once
/// after merging every physical relation.
mod execution;
pub use execution::run_optimised;
pub(crate) use execution::{
    direct_vector_retrieval, execute_relation_operator_tree_in_execution, execute_scored_tree,
    expect_posting_output, run_accelerated,
};

use uqa_execution::operator_tree::driver::posting::posting_list_to_scored;

pub(crate) fn estimate_operator_join_table_function(
    engine: &Engine,
    name: &str,
    relations: Option<&OperatorJoinRelations>,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<uqa_planner::LocalAccessEstimate> {
    let (relations, tree) = engine
        .retrieval_binding()
        .lower_join(name, relations, args, params)?;
    estimate_cross_relation_operator_join(engine, &relations, tree)
}
