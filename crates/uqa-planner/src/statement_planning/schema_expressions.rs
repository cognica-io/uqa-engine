//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Plan a stored expression copy with the same SQL inlining and lazy simplification as executable statements.

use super::executable::StatementPlanningContext;
use crate::optimizer::{optimize_scalar_expression, OptimizerConfig};
use uqa_sql::{
    ast::{ColumnDef, Expr, FunctionVolatility},
    schema::{
        expressions::{analyze_schema_expression, PlannedSchemaExpression},
        SchemaBindingContext, SchemaExpressionCatalog,
    },
    SQLError,
};

pub fn plan_schema_expression(
    context: &StatementPlanningContext<'_>,
    catalog: &dyn SchemaExpressionCatalog,
    expression: &Expr,
    columns: &[ColumnDef],
) -> Result<PlannedSchemaExpression, SQLError> {
    let mut analyzed = None;
    context.analysis.scopes.with_scope(&mut |scope| {
        analyzed = Some(analyze_schema_expression(
            &SchemaBindingContext {
                catalog,
                binding: &scope.binding_context()?,
            },
            expression,
            columns,
        )?);
        Ok(())
    })?;
    let mut analyzed = analyzed
        .ok_or_else(|| SQLError::Internal("schema expression analysis did not run".into()))?;
    let routines = context.optimization.routine_inlining().ok_or_else(|| {
        SQLError::Internal("schema expression planning requires routine metadata".into())
    })?;
    let mut config = OptimizerConfig::new(context.constant_evaluator);
    config.builtin_permissions = context.optimization.builtin_permissions();
    config.routine_inlining = Some(routines);
    config.coerced_conditionals = true;
    optimize_scalar_expression(&mut analyzed.scalar, &config)?;
    let immutable = routines.expression_volatility(&analyzed.scalar, &analyzed.schema)?
        == FunctionVolatility::Immutable;
    let constant = matches!(
        analyzed.scalar,
        uqa_sql::ScalarExpr::Literal(_) | uqa_sql::ScalarExpr::TypedLiteral { .. }
    );
    Ok(PlannedSchemaExpression {
        expression: analyzed.expression,
        ty: analyzed.ty,
        immutable,
        constant,
    })
}
