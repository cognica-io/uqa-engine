//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluate declared catalog and procedural expressions through physical scalar plans.

use crate::scalar::plan::{eval_physical, PhysicalEvalContext};
use crate::{
    query::{relational::QueryExpressionFactory, CteScope},
    PhysicalRow, RowSchema, RowSchemaExecution,
};
use uqa_core::Value;
use uqa_sql::{plan::ExpressionPlan, ResultRow, SQLError, SQLParam};

/// Lower an AST expression that belongs to a schema or procedural boundary,
/// then execute the resulting physical scalar IR. Runtime consumers never
/// invoke the AST evaluator or dispatch an AST subquery directly.
pub fn eval_lowered_expression<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    scope: CteScope<S>,
    expression: &uqa_sql::ast::Expr,
    row: Option<&ResultRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    eval_lowered_expression_with_type(factory, scope, expression, row, params)
        .map(|(value, _)| value)
}

/// Evaluate a standalone expression while retaining its declared SQL type.
/// Procedural statements such as `FOREACH` need the type because domains and
/// true arrays can share the same runtime value carrier.
pub fn eval_lowered_expression_with_type<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    mut scope: CteScope<S>,
    expression: &uqa_sql::ast::Expr,
    row: Option<&ResultRow>,
    params: &[SQLParam],
) -> Result<(Value, Option<uqa_sql::ast::ColumnType>), SQLError> {
    let mut expression = ExpressionPlan::lower(expression.clone());
    scope.scalar_subqueries.clone_from(&expression.subqueries);
    let hook = factory.bind_scope(scope);
    let declared_type = crate::scalar_type_with_resolver(
        &expression.scalar,
        &RowSchema::default(),
        params,
        hook.as_ref(),
    )?;
    expression.scalar = crate::bind_type_introspection_with_resolver(
        expression.scalar,
        &RowSchema::default(),
        params,
        hook.as_ref(),
    );
    let context = PhysicalEvalContext::new(row, params)
        .with_function_hook(hook.as_ref())
        .with_subquery_runner(hook.as_ref());
    let value = eval_physical(&expression, &context)?;
    Ok((value, declared_type))
}

/// Evaluate a catalog expression against a row while preserving the declared
/// SQL types of its columns. Values alone cannot distinguish, for example,
/// `smallint` from `integer`, so schema-owned expressions must bind before
/// they cross into the physical evaluator.
pub fn eval_lowered_expression_with_schema<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    mut scope: CteScope<S>,
    expression: &uqa_sql::ast::Expr,
    row: &ResultRow,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let mut expression = ExpressionPlan::lower(expression.clone());
    scope.scalar_subqueries.clone_from(&expression.subqueries);
    let hook = factory.bind_scope(scope);
    crate::scalar_type_with_resolver(&expression.scalar, schema, params, hook.as_ref())?;
    expression.scalar = crate::bind_type_introspection_with_resolver(
        expression.scalar,
        schema,
        params,
        hook.as_ref(),
    );
    let context = PhysicalEvalContext::new(Some(row), params)
        .with_row_schema(schema)
        .with_function_hook(hook.as_ref())
        .with_subquery_runner(hook.as_ref());
    eval_physical(&expression, &context)
}

/// Execute a catalog-bound scalar plan against one typed physical row while applying an independent relation-privilege subject to every nested query.
pub fn eval_stored_expression_plan_with_row<S: Clone + 'static>(
    factory: &dyn QueryExpressionFactory<S>,
    mut scope: CteScope<S>,
    expression: &ExpressionPlan,
    schema: &RowSchema,
    row: &PhysicalRow,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let mut expression = expression.clone();
    scope.scalar_subqueries.clone_from(&expression.subqueries);
    let hook = factory.bind_scope(scope);
    crate::scalar_type_with_resolver(&expression.scalar, schema, params, hook.as_ref())?;
    expression.scalar = crate::bind_type_introspection_with_resolver(
        expression.scalar,
        schema,
        params,
        hook.as_ref(),
    );
    let view = schema.view(row);
    let context = PhysicalEvalContext::from_row_lookup(&view, params)
        .with_row_schema(schema)
        .with_function_hook(hook.as_ref())
        .with_subquery_runner(hook.as_ref())
        .with_physical_outer_row(schema, row);
    eval_physical(&expression, &context)
}
