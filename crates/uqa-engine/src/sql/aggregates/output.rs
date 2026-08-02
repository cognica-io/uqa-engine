//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection and HAVING evaluation for one finalized aggregate group.

use super::{
    contains_aggregate, eval_scalar, expr_references_columns, exprs_match, group_context_row,
    projection_columns, replace_aggregates_with_values, resolve_having, AggregateAccumulator,
    CteScope, Engine, PlanSubqueryArena, QueryBlockPlan, ResultRow, SQLError, SQLParam,
    ScalarEvalContext, ScopedEngineHook, SpillBuffer, Value,
};

#[allow(clippy::too_many_arguments)]
pub(super) fn finish_group(
    engine: &Engine,
    statement: &QueryBlockPlan,
    accumulators: Vec<AggregateAccumulator>,
    group_values: &[Value],
    params: &[SQLParam],
    ctes: &CteScope,
    relaxed: bool,
) -> Result<Option<ResultRow>, SQLError> {
    let hook = ScopedEngineHook::new(engine, ctes);
    let subquery_arena = PlanSubqueryArena::new(&statement.subqueries, Some(&hook));
    let labels = projection_columns(&statement.projections);
    let group_row = group_context_row(statement, group_values);
    let mut row = ResultRow::new();
    let mut aggregate_index = 0;

    for (index, projection) in statement.projections.iter().enumerate() {
        let label = labels[index].clone();
        if contains_aggregate(engine, &projection.expr) {
            let resolved = replace_aggregates_with_values(
                engine,
                &projection.expr,
                &accumulators,
                &mut aggregate_index,
            )?;
            let context = ScalarEvalContext::new(Some(&group_row), params)
                .with_function_hook(&hook)
                .with_subquery_runner(&subquery_arena);
            row.insert(label, eval_scalar(&resolved, &context)?);
            continue;
        }
        if !expr_references_columns(&projection.expr) {
            let context = ScalarEvalContext::new(Some(&group_row), params)
                .with_function_hook(&hook)
                .with_subquery_runner(&subquery_arena);
            row.insert(label, eval_scalar(&projection.expr, &context)?);
            continue;
        }
        if let Some((_, value)) = statement
            .group_by
            .iter()
            .zip(group_values)
            .find(|(group, _)| exprs_match(&projection.expr, group))
        {
            row.insert(label, value.clone());
        } else if relaxed {
            row.insert(label, Value::Null);
        } else {
            return Err(SQLError::Unsupported(format!(
                "non-aggregated projection `{label}` must appear in GROUP BY"
            )));
        }
    }

    if let Some(having) = statement.having.as_ref() {
        let resolved = resolve_having(
            engine,
            having,
            &row,
            statement,
            &accumulators,
            group_values,
            params,
        )?;
        let mut having_row = group_row;
        having_row.extend(row.iter().map(|(key, value)| (key.clone(), value.clone())));
        let context = ScalarEvalContext::new(Some(&having_row), params)
            .with_function_hook(&hook)
            .with_subquery_runner(&subquery_arena);
        if !uqa_sql::expr::truthy(&eval_scalar(&resolved, &context)?) {
            return Ok(None);
        }
    }
    Ok(Some(row))
}

pub(super) fn push_output_row(
    output: &mut SpillBuffer,
    output_schema: &uqa_execution::RowSchema,
    pending: &mut Vec<ResultRow>,
    row: ResultRow,
) -> Result<(), SQLError> {
    pending.push(row);
    if pending.len() == uqa_execution::batch::DEFAULT_BATCH_SIZE {
        output
            .push(uqa_execution::Batch::new(
                output_schema.clone(),
                std::mem::take(pending),
            ))
            .map_err(super::sort_fallback::exec_to_sql_error)?;
        *pending = Vec::with_capacity(uqa_execution::batch::DEFAULT_BATCH_SIZE);
    }
    Ok(())
}

pub(super) fn flush_output_rows(
    output: &mut SpillBuffer,
    output_schema: &uqa_execution::RowSchema,
    pending: &mut Vec<ResultRow>,
) -> Result<(), SQLError> {
    if !pending.is_empty() {
        output
            .push(uqa_execution::Batch::new(
                output_schema.clone(),
                std::mem::take(pending),
            ))
            .map_err(super::sort_fallback::exec_to_sql_error)?;
    }
    Ok(())
}
