//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible validation for column default expressions.

use super::SchemaBindingContext;
use crate::ast::Expr;
use crate::RowSchema;
use crate::{ColumnType, SQLError};

pub fn validate_default_expression(
    context: &SchemaBindingContext<'_, '_>,
    expression: &mut Expr,
    target: &ColumnType,
) -> Result<(), SQLError> {
    let plan = crate::plan::ExpressionPlan::lower(expression.clone());
    if !plan.subqueries.is_empty() {
        return Err(default_error(
            "0A000",
            "cannot use subquery in DEFAULT expression",
        ));
    }
    if crate::semantics::windows::expr_has_window(&plan.scalar) {
        return Err(default_error(
            "42P20",
            "window functions are not allowed in DEFAULT expressions",
        ));
    }
    if crate::semantics::aggregates::contains_aggregate(context.catalog, &plan.scalar) {
        return Err(default_error(
            "42803",
            "aggregate functions are not allowed in DEFAULT expressions",
        ));
    }
    if crate::semantics::aggregates::expr_references_columns(&plan.scalar) {
        return Err(default_error(
            "0A000",
            "cannot use column reference in DEFAULT expression",
        ));
    }
    crate::type_resolution::scalar_type_with_resolver(
        &plan.scalar,
        &RowSchema::default(),
        &[],
        context.catalog,
    )?;
    crate::catalog::regrole_dependencies::reject_stored_regrole_constants(
        context.catalog,
        expression,
        Some(target),
    )?;
    bind_stored_schema_expression_routines(context, expression, expression.clone())?;
    Ok(())
}

pub fn bind_stored_schema_expression_routines(
    context: &SchemaBindingContext<'_, '_>,
    expression: &mut Expr,
    typed_expression: Expr,
) -> Result<bool, SQLError> {
    let mut plan = crate::plan::ExpressionPlan::lower_with(typed_expression, &|name: &str| {
        context.catalog.has_registered_aggregate_function(name)
    });
    crate::binding::bind_expression_plan_routines_for_storage(
        context.catalog,
        &mut plan,
        &[],
        context.binding,
        &RowSchema::default(),
    )?;
    let references = crate::binding::stored_routines::collect_expression_routine_references(&plan)?;
    crate::catalog::stored_ast::bind_stored_expression_routines(expression, &references)
}

fn default_error(sqlstate: &str, message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
