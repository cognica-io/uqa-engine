//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind SQL operator-join arguments before execution owns both relation operands.

use super::{
    lower_operator_join_table_function, DriverResult, Engine, GeneralizedPostingList, SQLParam,
    ScalarExpr,
};
use uqa_sql::ast::OperatorJoinRelations;

pub(crate) fn execute_operator_join_table_function(
    engine: &Engine,
    name: &str,
    relations: Option<&OperatorJoinRelations>,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<GeneralizedPostingList> {
    let (relations, tree) =
        lower_operator_join_table_function(engine, name, relations, args, params)?;
    uqa_execution::operator_tree::joins::execute_cross_relation_operator_join(
        &engine.tree_execution_context(),
        &relations,
        params,
        &tree,
    )
}
