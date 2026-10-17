//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Physical assembly for VALUES sources.

/// Build the physical operator for a VALUES source.
use super::{
    eval_scalar, filter_closed_source, qualify_source_operator_with_columns, ColumnPrune, CteScope,
    PhysicalOperator, PlanSubqueryArena, QualifierFilters, SQLError, SQLParam, ScalarEvalContext,
    ScalarExpr, SourceContext, SourcePlan,
};
use std::sync::Arc;

#[cfg(test)]
mod tests;

pub(super) fn build_values_source_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    build_values_source(
        context.ctes.routines,
        context.types,
        from,
        params,
        ctes,
        || context.relational.expression_scope(ctes.clone()),
        prune,
        filters,
        || context.relational.evaluator(params, ctes),
    )
}

/// Build a VALUES list with the predicates the planner placed on its qualifier. The planner removes those conjuncts from the query block's residual filter, so the list applies them to its own rows like every other source.
#[expect(
    clippy::too_many_arguments,
    reason = "borrows the caller's scopes and its source predicate placement"
)]
fn build_values_source<'a, S: Clone + 'static>(
    routines: &dyn uqa_sql::routines::RoutineResolution,
    types: &dyn crate::FunctionTypeResolver,
    from: &SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
    expression_scope: impl FnOnce() -> Arc<dyn crate::query::set_projection::SetFunctionRuntime + 'a>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
    evaluator: impl FnOnce() -> crate::SharedExpressionEvaluator<'a>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    match from {
        SourcePlan::Values {
            rows,
            alias,
            column_aliases,
            internal_relation,
            internal_column_types,
            ..
        } => {
            let internal_schema = from.internal_values_schema()?;
            let schema = crate::query::binding::values_schema_in_scope(
                routines,
                rows,
                &ctes.scalar_subqueries,
                None,
                params,
                ctes,
            )?;
            let column_types = if internal_relation.is_some() {
                if !alias.is_none() || !column_aliases.is_empty() {
                    return Err(SQLError::Internal(
                        "internal VALUES carrier has SQL-visible aliases".into(),
                    ));
                }
                if !rows.is_empty()
                    && rows
                        .iter()
                        .any(|row| row.len() != internal_column_types.len())
                {
                    return Err(SQLError::Internal(
                        "internal VALUES carrier row width does not match its declared attributes"
                            .into(),
                    ));
                }
                internal_column_types.as_slice()
            } else {
                schema.column_types()
            };
            let source_columns = schema.columns().to_vec();
            let hook = expression_scope();
            let rows =
                build_values_physical_rows(types, hook.as_ref(), params, rows, column_types)?;
            if let Some(schema) = internal_schema {
                return Ok(Box::new(crate::TableScan::from_physical_rows(schema, rows)));
            }
            let operator: Box<dyn crate::PhysicalOperator> =
                Box::new(crate::TableScan::from_physical_rows(schema, rows));
            let operator = qualify_source_operator_with_columns(
                operator,
                &source_columns,
                alias.as_deref().unwrap_or_default(),
                prune,
                column_aliases,
                ctes.lock_identities.emit,
            );
            Ok(match alias.as_deref() {
                Some(qualifier) => filter_closed_source(operator, qualifier, filters, evaluator),
                None => operator,
            })
        }
        _ => unreachable!("VALUES source builder called for a different source kind"),
    }
}

fn build_values_physical_rows(
    types: &dyn crate::FunctionTypeResolver,
    hook: &dyn crate::scalar::plan::QueryExpressionContext,
    params: &[SQLParam],
    rows: &[Vec<ScalarExpr>],
    column_types: &[Option<uqa_sql::ast::ColumnType>],
) -> Result<Vec<crate::PhysicalRow>, SQLError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let subquery_arena = PlanSubqueryArena::new(hook.subquery_plans(), Some(hook));
    let ctx = ScalarEvalContext::new(None, params)
        .with_function_hook(hook)
        .with_subquery_runner(&subquery_arena);
    let empty_schema = crate::RowSchema::default();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut values = Vec::with_capacity(row.len());
        for (i, expr) in row.iter().enumerate() {
            let source_type =
                crate::common_context_expression_type(expr, &empty_schema, params, Some(types))?;
            let v = uqa_sql::coerce_common_context_value(
                eval_scalar(expr, &ctx)?,
                source_type.as_ref(),
                column_types.get(i).and_then(Option::as_ref),
            )?;
            values.push(v);
        }
        out.push(crate::PhysicalRow::from_values(values));
    }
    Ok(out)
}
