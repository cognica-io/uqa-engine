//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind and evaluate expressions against mutation row images and the active CTE scope.
use super::rows::context::MutationExpressionContext;
use crate::{
    query::CteScope,
    scalar::plan::{eval_physical_scalar, PhysicalEvalContext},
    OwnedPhysicalRow, RowSchema, ScalarExpr,
};
use std::collections::BTreeSet;
use uqa_core::Value;
use uqa_sql::{SQLError, SQLParam};

pub fn eval_mutation_expr<S: Clone + 'static>(
    services: MutationExpressionContext<'_, S>,
    ctes: &CteScope<S>,
    expression: &ScalarExpr,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let hook = services.expressions.bind_scope(ctes.clone());
    let empty_schema = RowSchema::default();
    let schema = row.map_or(&empty_schema, |row| &row.schema);
    crate::scalar_type_with_resolver(expression, schema, params, services.types)?;
    let expression = crate::bind_type_introspection_with_resolver(
        expression.clone(),
        schema,
        params,
        services.types,
    );
    if let Some(row) = row {
        let view = row.view();
        let context = PhysicalEvalContext::from_row_lookup(&view, params)
            .with_function_hook(hook.as_ref())
            .with_subquery_runner(hook.as_ref())
            .with_physical_outer_row(&row.schema, &row.row);
        eval_physical_scalar(&expression, &ctes.scalar_subqueries, &context)
    } else {
        let context = PhysicalEvalContext::new(None, params)
            .with_function_hook(hook.as_ref())
            .with_subquery_runner(hook.as_ref());
        eval_physical_scalar(&expression, &ctes.scalar_subqueries, &context)
    }
}

pub fn row_independent_mutation_qualification_count<S: Clone + 'static>(
    services: MutationExpressionContext<'_, S>,
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Option<usize>, SQLError> {
    let Some(predicate) = predicate else {
        return Ok(Some(1));
    };
    let mut columns = BTreeSet::new();
    if !predicate.collect_columns(&mut columns) || !columns.is_empty() {
        return Ok(None);
    }
    Ok(Some(usize::from(uqa_sql::expr::truthy(
        &eval_mutation_expr(services, ctes, predicate, None, params)?,
    ))))
}
