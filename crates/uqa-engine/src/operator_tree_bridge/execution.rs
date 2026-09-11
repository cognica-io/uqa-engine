//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    operator_execution_error, posting_list_to_scored, DriverResult, Engine, EngineDriver,
    OperatorOutput, OperatorTree, SQLError, SQLParam, ScalarExpr, ScoredEntry,
};

/// Bind, optimize and execute a predicate below its existing statement boundary.
pub fn run_optimised(
    engine: &Engine,
    table: &str,
    where_expr: Option<&ScalarExpr>,
    params: &[SQLParam],
) -> Result<Option<Vec<ScoredEntry>>, SQLError> {
    engine
        .retrieval_query_context()
        .optimized(table, where_expr, params)
}

/// Optimise and execute an already-lowered tree through the same
/// planner/runtime boundary used by SQL `WHERE` lowering. Graph table
/// functions use this entry point too, so they do not maintain a
/// second physical dispatch implementation for nodes represented by
/// [`OperatorTree`].
pub(crate) fn execute_operator_tree(
    engine: &Engine,
    table: &str,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<OperatorOutput> {
    let _statement = engine.runtime.statement_gate.lock();
    execute_operator_tree_gated(engine, table, params, tree)
}

fn execute_operator_tree_gated(
    engine: &Engine,
    table: &str,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<OperatorOutput> {
    // Bayesian auto-calibration is a catalog write even though the enclosing
    // operator is a retrieval node. Direct API calls have no SQL statement
    // classifier to open a write transaction, and memory engines also need a
    // writable snapshot so a later physical-node failure cannot leave the
    // calibration behind. Existing SQL/user transactions already own the
    // appropriate frame and must not be nested here.
    if engine.transaction_depth() == 0 && tree_may_persist_calibration(tree) {
        return engine
            .transaction(|engine| execute_operator_tree_inner(engine, table, table, params, tree));
    }
    engine
        .with_graph_read_snapshot(|engine| {
            Ok(execute_operator_tree_inner(
                engine, table, table, params, tree,
            ))
        })
        .map_err(|error| operator_execution_error("pin operator snapshot", error))?
}

/// A direct physical driver call owns one snapshot for the whole tree,
/// including graph-aware propagation and parallel child operators.
pub(super) fn execute_public_physical_node(
    driver: &EngineDriver<'_>,
    tree: &OperatorTree,
) -> DriverResult<OperatorOutput> {
    use super::OperatorTreeDriver as _;
    let engine = driver.engine;
    let _statement = engine.runtime.statement_gate.lock();
    let execute = |engine: &Engine| {
        let scoped = engine
            .physical_retrieval_driver(driver.table, driver.table, driver.params)
            .with_parallel(driver.parallel.clone());
        scoped.execute_node(tree)
    };
    if engine.transaction_depth() == 0 && tree_may_persist_calibration(tree) {
        engine.transaction(execute)
    } else {
        engine
            .with_graph_read_snapshot(|engine| Ok(execute(engine)))
            .map_err(|error| operator_execution_error("pin physical operator snapshot", error))?
    }
}

fn execute_operator_tree_inner(
    engine: &Engine,
    table: &str,
    signal_table: &str,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<OperatorOutput> {
    uqa_execution::operator_tree::runtime::optimize_and_execute_tree(
        &engine.tree_execution_context(),
        table,
        signal_table,
        params,
        tree,
    )
}

/// Execute a concrete retrieval tree through the optimizer/plan-executor
/// boundary and convert its posting carrier for public engine APIs.
pub(crate) fn execute_scored_tree(
    engine: &Engine,
    table: &str,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<Vec<ScoredEntry>> {
    let output = execute_operator_tree(engine, table, params, tree)?;
    let posting = expect_posting_output(output, "retrieval API")?;
    Ok(posting_list_to_scored(&posting))
}

use uqa_execution::operator_tree::runtime::expect_posting_output;
use uqa_execution::operator_tree::runtime::tree_may_persist_calibration;

#[cfg(test)]
#[path = "transaction_boundary_tests.rs"]
mod transaction_boundary_tests;
