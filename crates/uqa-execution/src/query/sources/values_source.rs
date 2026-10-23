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
    params: &'a [SQLParam],
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
            let outer = ctes.row_lock_outer_row();
            let schema = crate::query::binding::values_schema_in_scope(
                routines,
                rows,
                &ctes.scalar_subqueries,
                outer.map(|row| &row.schema),
                params,
                ctes,
            )?;
            let internal_schema = from.internal_values_schema_with_inference(Some(&schema))?;
            let internal_types;
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
                internal_types = (0..internal_column_types.len())
                    .map(|position| {
                        internal_schema
                            .as_ref()
                            .and_then(|schema| schema.physical_type(position))
                            .cloned()
                    })
                    .collect::<Vec<_>>();
                internal_types.as_slice()
            } else {
                schema.column_types()
            };
            let source_columns = schema.columns().to_vec();
            let hook = expression_scope();
            let operator = build_values_scan(
                types,
                hook,
                params,
                rows,
                column_types,
                outer.cloned(),
                internal_schema.clone().unwrap_or_else(|| schema.clone()),
            )?;
            if internal_schema.is_some() {
                return Ok(operator);
            }
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

/// Bind all input types before execution, then evaluate only rows pulled from the VALUES source. The owned expression rows keep their addresses stable while a source is suspended.
pub(crate) fn build_values_scan<'a>(
    types: &dyn crate::FunctionTypeResolver,
    hook: Arc<dyn crate::query::set_projection::SetFunctionRuntime + 'a>,
    params: &'a [SQLParam],
    rows: &[Vec<ScalarExpr>],
    column_types: &[Option<uqa_sql::ast::ColumnType>],
    outer: Option<crate::OwnedPhysicalRow>,
    schema: crate::RowSchema,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let empty_schema = crate::RowSchema::default();
    let source_schema = outer.as_ref().map_or(&empty_schema, |row| &row.schema);
    let source_types = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|expression| {
                    crate::common_context_expression_type(
                        expression,
                        source_schema,
                        params,
                        Some(types),
                    )
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let column_types = column_types.to_vec();
    let expressions = rows.to_vec();
    let rows = expressions
        .into_iter()
        .zip(source_types)
        .map(move |(row, source_types)| {
            let subquery_arena = PlanSubqueryArena::new(hook.subquery_plans(), Some(hook.as_ref()));
            let ctx = outer
                .as_ref()
                .map_or_else(
                    || ScalarEvalContext::new(None, params),
                    |outer| {
                        ScalarEvalContext::from_row_lookup(outer, params)
                            .with_row_schema(&outer.schema)
                    },
                )
                .with_function_hook(hook.as_ref())
                .with_subquery_runner(&subquery_arena);
            let values = row
                .iter()
                .enumerate()
                .map(|(position, expression)| {
                    uqa_sql::coerce_common_context_value(
                        eval_scalar(expression, &ctx)?,
                        source_types[position].as_ref(),
                        column_types.get(position).and_then(Option::as_ref),
                    )
                })
                .collect::<Result<Vec<_>, SQLError>>()?;
            Ok(crate::PhysicalRow::from_values(values))
        });
    Ok(Box::new(crate::PhysicalRowIteratorScan::new(
        schema,
        Box::new(rows),
    )))
}
