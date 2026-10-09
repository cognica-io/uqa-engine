//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded sort aggregation for non-mergeable aggregate states.

use super::{
    aggregate_accumulator_templates, aggregate_targets, eval_scalar,
    instantiate_aggregate_accumulators, observe_aggregate, AggregateAccumulator, PlanSubqueryArena,
    QueryBlockPlan, QueryExpressionContext, SQLError, SQLParam, ScalarEvalContext, ScalarExpr,
    SpillBuffer, Value,
};
use crate::RowSchemaExecution;
use crate::{ExternalSort, PhysicalOperator, RowSchema, SortKey, SpillScan};

#[expect(
    clippy::too_many_arguments,
    reason = "keeps execution context inputs aligned"
)]
#[expect(
    clippy::too_many_lines,
    reason = "preserves aggregate NULL and type order"
)]
pub(super) fn aggregate_sorted_input(
    context: &dyn QueryExpressionContext,
    statement: &QueryBlockPlan,
    input: SpillBuffer,
    input_schema: &RowSchema,
    output_schema: &RowSchema,
    params: &[SQLParam],

    phase_budget: usize,
    relaxed: bool,
) -> Result<SpillBuffer, SQLError> {
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
    let evaluator = context.expression_evaluator(params);
    let mut sorted = ExternalSort::new(scan, keys, evaluator, None, phase_budget);
    sorted.open().map_err(exec_to_sql_error)?;

    let hook = context;
    let enums = uqa_sql::expr::EngineHook::enum_labels(context);
    let subquery_arena = PlanSubqueryArena::new(&statement.subqueries, Some(hook));
    let aggregate_targets = aggregate_targets(context, statement)
        .into_iter()
        .map(|target| {
            crate::bind_type_introspection_with_resolver(
                target.clone(),
                input_schema,
                params,
                context,
            )
        })
        .collect::<Vec<_>>();
    let aggregate_targets = crate::scalar::PreparedExpressions::scalars(aggregate_targets);
    let group_expressions = crate::scalar::PreparedExpressions::scalars(
        statement
            .group_by
            .iter()
            .map(|expression| {
                crate::bind_type_introspection_with_resolver(
                    expression.clone(),
                    input_schema,
                    params,
                    context,
                )
            })
            .collect(),
    );
    let output_plan = super::output::AggregateOutputPlan::compile(
        context,
        statement,
        &aggregate_targets,
        relaxed,
        input_schema,
        params,
    )?;
    let templates =
        aggregate_accumulator_templates(context, &aggregate_targets, input_schema, params)?;
    let accumulator_budget = (phase_budget / aggregate_targets.len().max(1)).max(1);
    let mut current_key: Option<Vec<Value>> = None;
    let mut current_accumulators = Vec::new();
    let mut output = SpillBuffer::new(phase_budget);
    let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);

    let execution = (|| -> Result<(), SQLError> {
        while let Some(batch) = sorted.next().map_err(exec_to_sql_error)? {
            for row in batch.rows {
                let view = batch.schema.view(&row);
                let scalar_context = ScalarEvalContext::from_row_lookup(&view, params)
                    .with_row_schema(&batch.schema)
                    .with_function_hook(hook)
                    .with_subquery_runner(&subquery_arena)
                    .with_physical_outer_row(&batch.schema, &row);
                let key = group_expressions
                    .iter()
                    .map(|expr| {
                        eval_scalar(
                            expr,
                            &scalar_context.with_function_states(group_expressions.calls()),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;

                if current_key
                    .as_ref()
                    .map(|current| crate::relational::equal_sort_key_values(current, &key, enums))
                    .transpose()
                    .map_err(exec_to_sql_error)?
                    .is_some_and(|equal| !equal)
                {
                    let finished_key = current_key.take().ok_or_else(|| {
                        SQLError::Internal("streaming aggregate lost its group key".into())
                    })?;
                    if let Some(row) = super::output::finish_group(
                        context,
                        statement,
                        &output_plan,
                        std::mem::take(&mut current_accumulators),
                        &finished_key,
                        output_schema.columns(),
                        params,
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
                    current_accumulators =
                        instantiate_aggregate_accumulators(&templates, accumulator_budget);
                }
                observe_targets(
                    &mut current_accumulators,
                    &aggregate_targets,
                    &scalar_context.with_function_states(aggregate_targets.calls()),
                )?;
            }
        }

        if let Some(key) = current_key.take() {
            if let Some(row) = super::output::finish_group(
                context,
                statement,
                &output_plan,
                current_accumulators,
                &key,
                output_schema.columns(),
                params,
            )? {
                super::output::push_output_row(&mut output, output_schema, &mut pending, row)?;
            }
        } else if statement.group_by.is_empty() {
            let accumulators = instantiate_aggregate_accumulators(&templates, accumulator_budget);
            if let Some(row) = super::output::finish_group(
                context,
                statement,
                &output_plan,
                accumulators,
                &[],
                output_schema.columns(),
                params,
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

pub(super) fn exec_to_sql_error(error: crate::ExecError) -> SQLError {
    match error {
        crate::ExecError::SQL(error) => error,
        crate::ExecError::Other(message) => SQLError::Internal(message),
    }
}
