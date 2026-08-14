//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded sort aggregation for non-mergeable aggregate states.

use super::{
    aggregate_targets, eval_scalar, new_aggregate_accumulators_with_budget, observe_aggregate,
    AggregateAccumulator, CteScope, Engine, PlanSubqueryArena, QueryBlockPlan, SQLError, SQLParam,
    ScalarEvalContext, ScalarExpr, ScopedEngineHook, SpillBuffer, Value,
};
use uqa_execution::{ExternalSort, PhysicalOperator, RowSchema, SortKey, SpillScan};

#[allow(clippy::too_many_arguments)]
pub(super) fn aggregate_sorted_input(
    engine: &Engine,
    statement: &QueryBlockPlan,
    input: SpillBuffer,
    input_schema: &RowSchema,
    output_schema: &RowSchema,
    params: &[SQLParam],
    ctes: &CteScope,
    phase_budget: usize,
    relaxed: bool,
) -> Result<SpillBuffer, SQLError> {
    use super::super::select::EngineExpressionEvaluator;

    let scan: Box<dyn PhysicalOperator + '_> =
        Box::new(SpillScan::new(input_schema.clone(), input));
    let keys = statement
        .group_by
        .iter()
        .cloned()
        .map(|expr| SortKey {
            expr,
            descending: false,
            nulls_first: None,
        })
        .collect();
    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    let mut sorted = ExternalSort::new(scan, keys, evaluator, None, phase_budget);
    sorted.open().map_err(exec_to_sql_error)?;

    let hook = ScopedEngineHook::new(engine, ctes);
    let subquery_arena = PlanSubqueryArena::new(&statement.subqueries, Some(&hook));
    let aggregate_targets = aggregate_targets(engine, statement)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let output_plan = super::output::AggregateOutputPlan::compile(
        engine,
        statement,
        &aggregate_targets,
        relaxed,
    )?;
    let accumulator_budget = (phase_budget / aggregate_targets.len().max(1)).max(1);
    let mut current_key: Option<Vec<Value>> = None;
    let mut current_accumulators = Vec::new();
    let mut output = SpillBuffer::new(phase_budget);
    let mut pending = Vec::with_capacity(uqa_execution::batch::DEFAULT_BATCH_SIZE);

    let execution = (|| -> Result<(), SQLError> {
        while let Some(batch) = sorted.next().map_err(exec_to_sql_error)? {
            for row in batch.rows {
                let view = batch.schema.view(&row);
                let context = ScalarEvalContext::from_row_lookup(&view, params)
                    .with_function_hook(&hook)
                    .with_subquery_runner(&subquery_arena);
                let key = statement
                    .group_by
                    .iter()
                    .map(|expr| eval_scalar(expr, &context))
                    .collect::<Result<Vec<_>, _>>()?;

                if current_key.as_ref().is_some_and(|current| current != &key) {
                    let finished_key = current_key.take().ok_or_else(|| {
                        SQLError::Internal("streaming aggregate lost its group key".into())
                    })?;
                    if let Some(row) = super::output::finish_group(
                        engine,
                        statement,
                        &output_plan,
                        std::mem::take(&mut current_accumulators),
                        &finished_key,
                        output_schema.columns(),
                        params,
                        ctes,
                    )? {
                        super::output::push_output_row(
                            &mut output,
                            output_schema,
                            &mut pending,
                            row,
                        )?;
                    }
                }
                if current_key.is_none() {
                    current_key = Some(key);
                    current_accumulators = new_aggregate_accumulators_with_budget(
                        engine,
                        &aggregate_targets,
                        accumulator_budget,
                    )?;
                }
                observe_targets(&mut current_accumulators, &aggregate_targets, &context)?;
            }
        }

        if let Some(key) = current_key.take() {
            if let Some(row) = super::output::finish_group(
                engine,
                statement,
                &output_plan,
                current_accumulators,
                &key,
                output_schema.columns(),
                params,
                ctes,
            )? {
                super::output::push_output_row(&mut output, output_schema, &mut pending, row)?;
            }
        } else if statement.group_by.is_empty() {
            let accumulators = new_aggregate_accumulators_with_budget(
                engine,
                &aggregate_targets,
                accumulator_budget,
            )?;
            if let Some(row) = super::output::finish_group(
                engine,
                statement,
                &output_plan,
                accumulators,
                &[],
                output_schema.columns(),
                params,
                ctes,
            )? {
                super::output::push_output_row(&mut output, output_schema, &mut pending, row)?;
            }
        }
        super::output::flush_output_rows(&mut output, output_schema, &mut pending)
    })();
    let close = sorted.close().map_err(exec_to_sql_error);
    combine_execution_and_close(execution, close, "aggregate sort")?;
    Ok(output)
}

pub(super) fn observe_targets(
    accumulators: &mut [AggregateAccumulator],
    aggregate_targets: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
) -> Result<(), SQLError> {
    for (index, expression) in aggregate_targets.iter().enumerate() {
        observe_target(&mut accumulators[index], expression, context)?;
    }
    Ok(())
}

pub(super) fn observe_target(
    accumulator: &mut AggregateAccumulator,
    expression: &ScalarExpr,
    context: &ScalarEvalContext<'_>,
) -> Result<(), SQLError> {
    let ScalarExpr::Func {
        name,
        args,
        distinct,
        order_by,
        filter,
        ..
    } = expression
    else {
        return Ok(());
    };
    if let Some(filter) = filter.as_deref() {
        if !uqa_sql::expr::truthy(&eval_scalar(filter, context)?) {
            return Ok(());
        }
    }
    observe_aggregate(accumulator, name, args, *distinct, order_by, context)
}

pub(super) fn combine_execution_and_close(
    execution: Result<(), SQLError>,
    close: Result<(), SQLError>,
    operator: &str,
) -> Result<(), SQLError> {
    match (execution, close) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) | (Err(error), Ok(())) => Err(error),
        (Err(execution_error), Err(close_error)) => Err(SQLError::Internal(format!(
            "{execution_error}; closing {operator} after failure also failed: {close_error}"
        ))),
    }
}

pub(super) fn exec_to_sql_error(error: uqa_execution::ExecError) -> SQLError {
    match error {
        uqa_execution::ExecError::SQL(error) => error,
        uqa_execution::ExecError::Other(message) => SQLError::Internal(message),
    }
}
