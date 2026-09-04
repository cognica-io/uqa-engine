//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible validation for column default expressions.

use super::{ColumnType, Engine, SQLError};
use uqa_execution::RowSchema;
use uqa_sql::ast::Expr;

pub(super) fn validate_default_expression(
    engine: &Engine,
    expression: &mut Expr,
    target: &ColumnType,
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
    crate::sql::reject_stored_regrole_constants(engine, expression, Some(target))?;
    bind_stored_schema_expression_routines(engine, expression, expression.clone())?;
    Ok(())
}

pub(crate) fn bind_stored_schema_expression_routines(
    engine: &Engine,
    expression: &mut Expr,
    typed_expression: Expr,
) -> Result<bool, SQLError> {
    let mut plan = uqa_planner::ExpressionPlan::lower_with(typed_expression, &|name: &str| {
        engine.has_registered_aggregate_function(name)
    });
    crate::sql::bind_catalog_expression_routines_with_outer(
        engine,
        &mut plan,
        &[],
        &RowSchema::default(),
    )?;
    let references = crate::sql::collect_expression_routine_references(&plan)?;
    crate::engine_events::bind_stored_expression_routines(expression, &references)
}

fn default_error(sqlstate: &str, message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
