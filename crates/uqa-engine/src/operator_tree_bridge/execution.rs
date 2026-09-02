//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    engine_query_optimizer, lower_where_bound, operator_execution_error, posting_list_to_scored,
    DirectVectorRetrieval, DriverResult, Engine, EngineDriver, OperatorOutput, OperatorTree,
    PlanExecutor, PostingList, SQLError, SQLParam, ScalarExpr, ScoredEntry, TextScoringMode,
};

pub(crate) fn direct_vector_retrieval(
    engine: &Engine,
    expression: &ScalarExpr,
    params: &[SQLParam],
) -> Result<Option<DirectVectorRetrieval>, SQLError> {
    let Some(tree) = lower_where_bound(engine, expression, params)? else {
        return Ok(None);
    };
    Ok(match tree {
        OperatorTree::KNN { k, .. } => Some(DirectVectorRetrieval::Knn { top_k: k }),
        OperatorTree::CalibratedVectorMatch {
            field,
            query_vector,
            k,
            threshold,
        } => Some(DirectVectorRetrieval::Calibrated {
            field,
            query_vector,
            top_k: k,
            threshold,
        }),
        _ => None,
    })
}

/// The "lower -> optimise -> execute" pipeline. `Some(rows)` when the
/// WHERE expression maps cleanly onto the operator tree; `None` keeps the
/// predicate in the enclosing relational filter node. Any engine-side failure
/// returned by the helpers it re-uses bubbles up as `Err`.
pub fn run_optimised(
    engine: &Engine,
    table: &str,
    where_expr: Option<&ScalarExpr>,
    params: &[SQLParam],
) -> Result<Option<Vec<ScoredEntry>>, SQLError> {
    let Some(expr) = where_expr else {
        return Ok(None);
    };
    let Some(tree) = lower_where_bound(engine, expr, params)? else {
        return Ok(None);
    };
    let pl = expect_posting_output(
        execute_operator_tree_in_execution(engine, table, params, &tree)?,
        "SQL WHERE",
    )?;
    Ok(Some(posting_list_to_scored(&pl)))
}

/// SELECT access-path counterpart of [`run_optimised`]. A scalar predicate
/// only leaves the relational scan when optimization selected a real index;
/// retrieval operators always retain their posting-list execution path.
pub(crate) fn run_accelerated(
    engine: &Engine,
    table: &str,
    signal_table: &str,
    where_expr: Option<&ScalarExpr>,
    params: &[SQLParam],
) -> Result<Option<Vec<ScoredEntry>>, SQLError> {
    let Some(expression) = where_expr else {
        return Ok(None);
    };
    let Some(tree) = lower_where_bound(engine, expression, params)? else {
        return Ok(None);
    };
    let optimized = engine_query_optimizer(engine, table, &tree)?.optimize(tree);
    let mut has_index_scan = false;
    optimized.visit(&mut |node| has_index_scan |= matches!(node, OperatorTree::IndexScan { .. }));
    if !has_index_scan && !uqa_planner::optimizer::contains_retrieval(expression) {
        let mut filters = Vec::new();
        optimized.visit(&mut |node| {
            if let OperatorTree::Filter {
                field, predicate, ..
            } = node
            {
                filters.push((field.clone(), predicate.clone()));
            }
        });
        let mut all_value_indexed = !filters.is_empty();
        for (field, predicate) in filters {
            if !engine
                .value_index_supports(table, &field, &predicate)
                .map_err(|error| operator_execution_error("prepare value index", error))?
            {
                all_value_indexed = false;
                break;
            }
        }
        if !all_value_indexed {
            return Ok(None);
        }
    }
    let output = execute_preoptimized_operator_tree_in_execution(
        engine,
        table,
        signal_table,
        params,
        &optimized,
    )?;
    let posting = expect_posting_output(output, "SQL WHERE")?;
    Ok(Some(posting_list_to_scored(&posting)))
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
    execute_operator_tree_inner(engine, table, table, params, tree)
}

/// Execute below a SQL/direct statement boundary that already owns the
/// statement gate. Rayon workers use this entry point so they do not try to
/// acquire a thread-affine reentrant lock held by their coordinator.
pub(crate) fn execute_operator_tree_in_execution(
    engine: &Engine,
    table: &str,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<OperatorOutput> {
    if engine.transaction_depth() == 0 && tree_may_persist_calibration(tree) {
        return Err(SQLError::Internal(
            "calibrating operator execution requires an active statement transaction".into(),
        ));
    }
    execute_operator_tree_inner(engine, table, table, params, tree)
}

/// Execute a SQL retrieval tree against a canonical storage relation while retaining the relation spelling used to address its scoring signal.
pub(crate) fn execute_relation_operator_tree_in_execution(
    engine: &Engine,
    table: &str,
    signal_table: &str,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<OperatorOutput> {
    if engine.transaction_depth() == 0 && tree_may_persist_calibration(tree) {
        return Err(SQLError::Internal(
            "calibrating operator execution requires an active statement transaction".into(),
        ));
    }
    execute_operator_tree_inner(engine, table, signal_table, params, tree)
}

fn execute_operator_tree_inner(
    engine: &Engine,
    table: &str,
    signal_table: &str,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<OperatorOutput> {
    validate_text_top_k_placement(tree)?;
    let optimized = engine_query_optimizer(engine, table, tree)?.optimize(tree.clone());
    execute_preoptimized_operator_tree_inner(engine, table, signal_table, params, &optimized)
}

fn execute_preoptimized_operator_tree_in_execution(
    engine: &Engine,
    table: &str,
    signal_table: &str,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<OperatorOutput> {
    if engine.transaction_depth() == 0 && tree_may_persist_calibration(tree) {
        return Err(SQLError::Internal(
            "calibrating operator execution requires an active statement transaction".into(),
        ));
    }
    execute_preoptimized_operator_tree_inner(engine, table, signal_table, params, tree)
}

fn execute_preoptimized_operator_tree_inner(
    engine: &Engine,
    table: &str,
    signal_table: &str,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<OperatorOutput> {
    validate_text_top_k_placement(tree)?;
    let driver = EngineDriver::new_for_relation_in_execution(engine, table, signal_table, params);
    let mut executor = PlanExecutor::new(&driver);
    executor.execute(tree)
}

fn validate_text_top_k_placement(tree: &OperatorTree) -> DriverResult<()> {
    let root_is_physical_text = matches!(tree, OperatorTree::Term { top_k: Some(_), .. });
    let mut physical_text_nodes = 0_usize;
    tree.visit(&mut |node| {
        if matches!(node, OperatorTree::Term { top_k: Some(_), .. }) {
            physical_text_nodes += 1;
        }
    });
    if physical_text_nodes == usize::from(root_is_physical_text) {
        Ok(())
    } else {
        Err(SQLError::Internal(
            "physical text top-k is valid only as the root retrieval leaf".into(),
        ))
    }
}

fn tree_may_persist_calibration(tree: &OperatorTree) -> bool {
    let mut may_persist = false;
    tree.visit(&mut |node| {
        may_persist |= matches!(
            node,
            OperatorTree::BayesianScore { .. }
                | OperatorTree::Term {
                    scoring: Some(TextScoringMode::BayesianBM25),
                    ..
                }
                | OperatorTree::BayesianMatchWithPrior { .. }
                | OperatorTree::MultiFieldSearch { .. }
        );
    });
    may_persist
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

pub(crate) fn expect_posting_output(
    output: OperatorOutput,
    context: &str,
) -> DriverResult<PostingList> {
    match output {
        OperatorOutput::Posting(result) => Ok(result),
        OperatorOutput::Graph(result) => Ok(result.to_posting_list()),
        OperatorOutput::Generalized(_) => Err(SQLError::TypeMismatch(format!(
            "{context} requires single-document rows, but the physical plan produced join tuples"
        ))),
    }
}

/// Combine the corpus priors reported by fusion signals into the single
/// fusion-level prior: the mean of their logits. Every signal estimates
/// the same corpus-level P(relevant), so averaging in log-odds space
/// yields one prior no matter how many signals report it.
pub(crate) fn combine_signal_priors(priors: &[f64]) -> Option<f64> {
    if priors.is_empty() {
        return None;
    }
    let mean_logit = priors
        .iter()
        .map(|rate| uqa_scoring::logit(*rate))
        .sum::<f64>()
        / priors.len() as f64;
    Some(uqa_scoring::sigmoid(mean_logit))
}

#[cfg(test)]
#[path = "transaction_boundary_tests.rs"]
mod transaction_boundary_tests;
