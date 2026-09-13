//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retrieval-tree optimization, active-statement checks and physical execution.

use super::driver::{context::PhysicalDriverContext, PhysicalRetrievalDriver};
use super::{OperatorOutput, PlanExecutor};
use uqa_core::PostingList;
use uqa_operators::{OperatorTree, TextScoringMode};
use uqa_sql::{SQLError, SQLParam};

type DriverResult<T> = Result<T, SQLError>;

pub trait RetrievalPlanOptimizer: Sync {
    fn optimize(&self, table: &str, tree: &OperatorTree) -> DriverResult<OperatorTree>;
}

pub trait RetrievalTransactionState: Sync {
    fn transaction_depth(&self) -> usize;
}

#[derive(Clone, Copy)]
pub struct TreeExecutionContext<'a> {
    pub driver: PhysicalDriverContext<'a>,
    pub optimizer: &'a dyn RetrievalPlanOptimizer,
    pub transaction: &'a dyn RetrievalTransactionState,
}

/// Execute below an existing statement boundary; calibration must already have a transaction.
pub fn execute_tree(
    context: &TreeExecutionContext<'_>,
    table: &str,
    signal_table: &str,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<OperatorOutput> {
    if context.transaction.transaction_depth() == 0 && tree_may_persist_calibration(tree) {
        return Err(SQLError::Internal(
            "calibrating operator execution requires an active statement transaction".into(),
        ));
    }
    optimize_and_execute_tree(context, table, signal_table, params, tree)
}

pub fn optimize_and_execute_tree(
    context: &TreeExecutionContext<'_>,
    table: &str,
    signal_table: &str,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<OperatorOutput> {
    validate_text_top_k_placement(tree)?;
    let optimized = context.optimizer.optimize(table, tree)?;
    execute_physical_tree(context, table, signal_table, params, &optimized)
}

pub fn execute_preoptimized_tree(
    context: &TreeExecutionContext<'_>,
    table: &str,
    signal_table: &str,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<OperatorOutput> {
    if context.transaction.transaction_depth() == 0 && tree_may_persist_calibration(tree) {
        return Err(SQLError::Internal(
            "calibrating operator execution requires an active statement transaction".into(),
        ));
    }
    execute_physical_tree(context, table, signal_table, params, tree)
}

fn execute_physical_tree(
    context: &TreeExecutionContext<'_>,
    table: &str,
    signal_table: &str,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<OperatorOutput> {
    validate_text_top_k_placement(tree)?;
    let driver = PhysicalRetrievalDriver::new(context.driver, table, signal_table, params);
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

pub fn tree_may_persist_calibration(tree: &OperatorTree) -> bool {
    let mut may_persist = false;
    tree.visit(&mut |node| {
        may_persist |= matches!(
            node,
            OperatorTree::BayesianScore { .. }
                | OperatorTree::Term {
                    scoring: Some(TextScoringMode::BayesianBM25),
                    ..
                }
                | OperatorTree::Phrase {
                    scoring: Some(TextScoringMode::BayesianBM25),
                    ..
                }
                | OperatorTree::BayesianMatchWithPrior { .. }
                | OperatorTree::MultiFieldSearch { .. }
        );
    });
    may_persist
}

pub fn expect_posting_output(output: OperatorOutput, context: &str) -> DriverResult<PostingList> {
    match output {
        OperatorOutput::Posting(result) => Ok(result),
        OperatorOutput::Graph(result) => Ok(result.to_posting_list()),
        OperatorOutput::Generalized(_) => Err(SQLError::TypeMismatch(format!(
            "{context} requires single-document rows, but the physical plan produced join tuples"
        ))),
    }
}

#[cfg(test)]
mod tests;
