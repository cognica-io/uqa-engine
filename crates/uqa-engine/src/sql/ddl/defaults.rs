//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible validation for column default expressions.

use super::{Engine, SQLError};
use uqa_execution::RowSchema;
use uqa_sql::ast::Expr;

pub(super) fn validate_default_expression(
    engine: &Engine,
    expression: &Expr,
) -> Result<(), SQLError> {
    let plan = uqa_planner::ExpressionPlan::lower(expression.clone());
    if !plan.subqueries.is_empty() {
        return Err(default_error(
            "0A000",
            "cannot use subquery in DEFAULT expression",
        ));
    }
    if crate::sql::window::expr_has_window(&plan.scalar) {
        return Err(default_error(
            "42P20",
            "window functions are not allowed in DEFAULT expressions",
        ));
    }
    if crate::sql::aggregates::contains_aggregate(engine, &plan.scalar) {
        return Err(default_error(
            "42803",
            "aggregate functions are not allowed in DEFAULT expressions",
        ));
    }
    if crate::sql::aggregates::expr_references_columns(&plan.scalar) {
        return Err(default_error(
            "0A000",
            "cannot use column reference in DEFAULT expression",
        ));
    }
    uqa_execution::scalar_type_with_resolver(&plan.scalar, &RowSchema::default(), &[], engine)?;
    Ok(())
}

fn default_error(sqlstate: &str, message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
