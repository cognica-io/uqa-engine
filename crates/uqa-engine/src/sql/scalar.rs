//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adapt statement state to the physical scalar evaluator.

use uqa_core::Value;
use uqa_execution::{PhysicalRow, RowSchema};
use uqa_sql::plan::ExpressionPlan;
use uqa_sql::{ResultRow, SQLError, SQLParam};

use crate::Engine;

pub(crate) fn eval_lowered_expression(
    engine: &Engine,
    expression: &uqa_sql::ast::Expr,
    row: Option<&ResultRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_current_routine(engine);
    uqa_execution::query::catalog_expression::eval_lowered_expression(
        engine, scope, expression, row, params,
    )
}

pub(crate) fn eval_lowered_expression_with_type(
    engine: &Engine,
    expression: &uqa_sql::ast::Expr,
    row: Option<&ResultRow>,
    params: &[SQLParam],
) -> Result<(Value, Option<uqa_sql::ColumnType>), SQLError> {
    let scope = crate::capabilities::query_scope::new_for_current_routine(engine);
    uqa_execution::query::catalog_expression::eval_lowered_expression_with_type(
        engine, scope, expression, row, params,
    )
}

pub(crate) fn eval_lowered_expression_with_schema(
    engine: &Engine,
    expression: &uqa_sql::ast::Expr,
    row: &ResultRow,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_current_routine(engine);
    uqa_execution::query::catalog_expression::eval_lowered_expression_with_schema(
        engine, scope, expression, row, schema, params,
    )
}

pub(crate) fn eval_stored_expression_plan_with_row(
    engine: &Engine,
    expression: &ExpressionPlan,
    schema: &RowSchema,
    row: &PhysicalRow,
    params: &[SQLParam],
    privilege_subject: Option<&str>,
) -> Result<Value, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_statement(engine, privilege_subject);
    uqa_execution::query::catalog_expression::eval_stored_expression_plan_with_row(
        engine, scope, expression, schema, row, params,
    )
}
