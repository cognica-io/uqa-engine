//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retrieval predicates and row functions within an active query statement.

mod functions;

use super::{
    binding::RetrievalBinding,
    driver::posting::posting_list_to_scored,
    runtime::{
        execute_preoptimized_tree, execute_tree, expect_posting_output, TreeExecutionContext,
    },
};
use crate::query::graph_lifecycle::GraphLifecycle;
use uqa_core::ScoredEntry;
use uqa_operators::OperatorTree;
use uqa_sql::{SQLError, SQLParam, ScalarExpr};

/// Planner-owned access selection and physical text limits.
pub trait RelationRetrievalPlanner: Sync {
    fn accelerated_tree(
        &self,
        table: &str,
        expression: &ScalarExpr,
        tree: OperatorTree,
    ) -> Result<Option<OperatorTree>, SQLError>;
    fn text_top_k(
        &self,
        table: &str,
        tree: OperatorTree,
        top_k: usize,
    ) -> Result<OperatorTree, SQLError>;
}

/// Inputs for retrieval calls below the caller's statement and transaction boundary.
pub struct RetrievalQueryContext<'a> {
    pub binding: RetrievalBinding<'a>,
    pub trees: TreeExecutionContext<'a>,
    pub planner: &'a dyn RelationRetrievalPlanner,
    pub graphs: &'a dyn GraphLifecycle,
}
impl RetrievalQueryContext<'_> {
    pub fn optimized(
        &self,
        table: &str,
        where_expr: Option<&ScalarExpr>,
        params: &[SQLParam],
    ) -> Result<Option<Vec<ScoredEntry>>, SQLError> {
        let Some(expr) = where_expr else {
            return Ok(None);
        };
        let Some(tree) = self.binding.lower_where(expr, params)? else {
            return Ok(None);
        };
        let pl = expect_posting_output(
            execute_tree(&self.trees, table, table, params, &tree)?,
            "SQL WHERE",
        )?;
        Ok(Some(posting_list_to_scored(&pl)))
    }

    pub fn accelerated(
        &self,
        table: &str,
        signal_table: &str,
        where_expr: Option<&ScalarExpr>,
        params: &[SQLParam],
    ) -> Result<Option<Vec<ScoredEntry>>, SQLError> {
        let Some(expression) = where_expr else {
            return Ok(None);
        };
        let Some(tree) = self.binding.lower_where(expression, params)? else {
            return Ok(None);
        };
        let Some(optimized) = self.planner.accelerated_tree(table, expression, tree)? else {
            return Ok(None);
        };
        let output =
            execute_preoptimized_tree(&self.trees, table, signal_table, params, &optimized)?;
        let posting = expect_posting_output(output, "SQL WHERE")?;
        Ok(Some(posting_list_to_scored(&posting)))
    }
}
