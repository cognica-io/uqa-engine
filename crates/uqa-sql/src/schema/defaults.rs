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
    crate::catalog::stored_ast::fold_assigned_stored_literal(
        expression,
        target,
        crate::FunctionTypeResolver::enum_labels(context.catalog),
    )?;
    bind_stored_schema_expression(context, expression, expression.clone())?;
    Ok(())
}

/// Bind a copy of stored schema expression syntax and carry its exact routine identities, user-defined type identities and enum constants back into the syntax. `typed_expression` is the syntax with its column references replaced by typed placeholders.
pub fn bind_stored_schema_expression(
    context: &SchemaBindingContext<'_, '_>,
    expression: &mut Expr,
    typed_expression: Expr,
) -> Result<bool, SQLError> {
    let lowered = crate::plan::ExpressionPlan::lower_with(typed_expression, &|name: &str| {
        context.catalog.has_registered_aggregate_function(name)
    });
    let mut plan = lowered.clone();
    crate::binding::bind_expression_plan_routines_for_storage(
        context.catalog,
        &mut plan,
        &[],
        context.binding,
        &RowSchema::default(),
    )?;
    let sites = crate::binding::syntax_sites::expression_syntax_sites(&lowered, &plan)?;
    crate::catalog::stored_ast::bind_stored_expression_sites(expression, &sites)
}

fn default_error(sqlstate: &str, message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
