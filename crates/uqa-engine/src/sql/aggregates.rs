//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL aggregate execution and spill buffering for blocking inputs.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::sync::Arc;

use uqa_core::{DecimalValue, Value};
use uqa_execution::{
    eval_scalar, AggregateExecutor, Batch, ExecResult, ExternalSort, PhysicalOperator, RowSchema,
    ScalarEvalContext, ScalarExpr, ScalarOrder, SortKey, SpillBuffer, SpillScan,
};
use uqa_planner::{ProjectionPlan, QueryBlockPlan};
use uqa_sql::{ResultRow, SQLError, SQLParam};

use crate::{Engine, SQLAggregateFunction, SQLAggregateState};

use super::scalar::PlanSubqueryArena;
use super::{core_value_to_json, projection_columns, CteScope, ScopedEngineHook};

const AGGREGATE_MERGE_FAN_IN: usize = 16;

/// Engine aggregate adapter with a strict encoded-byte input budget.
///
/// Input is fanned out once per grouping set into disk-backed buffers. Each
/// buffer is externally sorted by its active grouping key and then folded one
/// group at a time, so neither the complete input nor a high-cardinality group
/// map is retained in memory.
pub(super) struct PhysicalAggregateExecutor<'a> {
    engine: &'a Engine,
    statement: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
    input_schema: Vec<String>,
    output_schema: RowSchema,
    work_mem_bytes: usize,
    inputs: Vec<SpillBuffer>,
}

impl<'a> PhysicalAggregateExecutor<'a> {
    pub(super) fn new(
        engine: &'a Engine,
        statement: &'a QueryBlockPlan,
        params: &'a [SQLParam],
        ctes: &'a CteScope,
        schema: Vec<String>,
        work_mem_bytes: usize,
    ) -> Self {
        let set_count = statement.grouping_sets.len().max(1);
        // One third is reserved for all retained input tails, one third for
        // external sorting, and one third for the active group's aggregate
        // state. Integer division is intentionally clamped to one byte so a
        // tiny test budget still exercises the disk path.
        let input_budget = (work_mem_bytes / 3 / set_count).max(1);
        let inputs = (0..set_count)
            .map(|_| SpillBuffer::new(input_budget))
            .collect();
        Self {
            engine,
            statement,
            params,
            ctes,
            input_schema: schema,
            output_schema: RowSchema::new(projection_columns(&statement.projections)),
            work_mem_bytes,
            inputs,
        }
    }
}

impl AggregateExecutor for PhysicalAggregateExecutor<'_> {
    fn consume(&mut self, batch: Batch) -> ExecResult<()> {
        let Some((last, prefix)) = self.inputs.split_last_mut() else {
            return Err(uqa_execution::ExecError::Other(
                "aggregate input fanout is empty".into(),
            ));
        };
        for input in prefix {
            input.push(batch.clone())?;
        }
        last.push(batch)?;
        Ok(())
    }

    fn finish(&mut self) -> ExecResult<SpillBuffer> {
        let output_budget = (self.work_mem_bytes / 3).max(1);
        let mut output = SpillBuffer::new(output_budget);
        let mut expected_output_rows = 0_usize;
        let inputs = std::mem::take(&mut self.inputs);
        for (set_index, input) in inputs.into_iter().enumerate() {
            let (mut statement, relaxed) = if self.statement.grouping_sets.is_empty() {
                (self.statement.clone(), false)
            } else {
                let mut statement = self.statement.clone();
                statement
                    .group_by
                    .clone_from(&self.statement.grouping_sets[set_index]);
                statement.grouping_sets.clear();
                (statement, true)
            };
            // ORDER BY/LIMIT belong after aggregation, never inside this
            // group-folding pass.
            statement.order_by.clear();
            statement.limit = None;
            statement.offset = None;
            let mut set_output = aggregate_spilled_set(
                self.engine,
                &statement,
                input,
                &self.input_schema,
                &self.output_schema,
                self.params,
                self.ctes,
                output_budget,
                relaxed,
            )?;
            let expected_rows = set_output.rows();
            expected_output_rows =
                expected_output_rows
                    .checked_add(expected_rows)
                    .ok_or_else(|| {
                        uqa_execution::ExecError::Other(
                            "aggregate output row count overflow".into(),
                        )
                    })?;
            let mut copied_rows = 0_usize;
            for batch in set_output.drain()? {
                let batch = batch?;
                copied_rows = copied_rows.checked_add(batch.rows.len()).ok_or_else(|| {
                    uqa_execution::ExecError::Other("aggregate copied row count overflow".into())
                })?;
                output.push(batch)?;
            }
            if copied_rows != expected_rows {
                return Err(uqa_execution::ExecError::Other(format!(
                    "aggregate spill drain returned {copied_rows} rows, expected {expected_rows}"
                )));
            }
        }
        if output.rows() != expected_output_rows {
            return Err(uqa_execution::ExecError::Other(format!(
                "aggregate output retained {} rows, expected {expected_output_rows}",
                output.rows()
            )));
        }
        Ok(output)
    }
}

#[allow(clippy::too_many_arguments)]
fn aggregate_spilled_set(
    engine: &Engine,
    statement: &QueryBlockPlan,
    input: SpillBuffer,
    input_schema: &[String],
    output_schema: &RowSchema,
    params: &[SQLParam],
    ctes: &CteScope,
    phase_budget: usize,
    relaxed: bool,
) -> Result<SpillBuffer, SQLError> {
    use super::select::EngineExpressionEvaluator;

    let scan: Box<dyn PhysicalOperator + '_> =
        Box::new(SpillScan::new(input_schema.to_vec(), input));
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
    let aggregate_targets = aggregate_exprs(engine, &statement.projections);
    let accumulator_budget = (phase_budget / aggregate_targets.len().max(1)).max(1);
    let mut current_key: Option<Vec<Value>> = None;
    let mut current_accumulators: Vec<AggregateAccumulator> = Vec::new();
    let mut output = SpillBuffer::new(phase_budget);
    let mut pending = Vec::with_capacity(uqa_execution::batch::DEFAULT_BATCH_SIZE);

    let execution = (|| -> Result<(), SQLError> {
        while let Some(batch) = sorted.next().map_err(exec_to_sql_error)? {
            for row in batch.rows {
                let context = ScalarEvalContext::new(Some(&row), params)
                    .with_function_hook(&hook)
                    .with_subquery_runner(&subquery_arena);
                let key = statement
                    .group_by
                    .iter()
                    .map(|expr| eval_scalar(expr, &context))
                    .collect::<Result<Vec<_>, _>>()?;

                if current_key.as_ref().is_some_and(|current| current != &key) {
                    let finished_key = current_key.take().ok_or_else(|| {
                        SQLError::Internal("streaming aggregate lost its current group key".into())
                    })?;
                    if let Some(row) = finish_stream_group(
                        engine,
                        statement,
                        std::mem::take(&mut current_accumulators),
                        &finished_key,
                        params,
                        ctes,
                        relaxed,
                    )? {
                        pending.push(row);
                        if pending.len() == uqa_execution::batch::DEFAULT_BATCH_SIZE {
                            output
                                .push(Batch::new(
                                    output_schema.clone(),
                                    std::mem::take(&mut pending),
                                ))
                                .map_err(exec_to_sql_error)?;
                            pending = Vec::with_capacity(uqa_execution::batch::DEFAULT_BATCH_SIZE);
                        }
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
                for (index, expression) in aggregate_targets.iter().enumerate() {
                    let ScalarExpr::Func {
                        name,
                        args,
                        distinct,
                        order_by,
                        filter,
                    } = expression
                    else {
                        continue;
                    };
                    if let Some(filter) = filter.as_deref() {
                        if !uqa_sql::expr::truthy(&eval_scalar(filter, &context)?) {
                            continue;
                        }
                    }
                    observe_aggregate(
                        &mut current_accumulators[index],
                        name,
                        args,
                        *distinct,
                        order_by,
                        &context,
                    )?;
                }
            }
        }

        if let Some(key) = current_key.take() {
            if let Some(row) = finish_stream_group(
                engine,
                statement,
                current_accumulators,
                &key,
                params,
                ctes,
                relaxed,
            )? {
                pending.push(row);
            }
        } else if statement.group_by.is_empty() {
            let accumulators = new_aggregate_accumulators_with_budget(
                engine,
                &aggregate_targets,
                accumulator_budget,
            )?;
            if let Some(row) =
                finish_stream_group(engine, statement, accumulators, &[], params, ctes, relaxed)?
            {
                pending.push(row);
            }
        }
        if !pending.is_empty() {
            output
                .push(Batch::new(
                    output_schema.clone(),
                    std::mem::take(&mut pending),
                ))
                .map_err(exec_to_sql_error)?;
        }
        Ok(())
    })();
    let close = sorted.close().map_err(exec_to_sql_error);
    combine_execution_and_close(execution, close, "aggregate sort")?;
    Ok(output)
}

fn combine_execution_and_close(
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

fn exec_to_sql_error(error: uqa_execution::ExecError) -> SQLError {
    match error {
        uqa_execution::ExecError::SQL(error) => error,
        uqa_execution::ExecError::Other(message) => SQLError::Internal(message),
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_stream_group(
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

/// Replace aggregates referenced by HAVING with the state already finalized
/// for this group. SQL currently requires a HAVING aggregate to also appear in
/// the projection, matching the prior executor's explicit boundary.
mod accumulator;
mod analysis;
mod distinct;
mod finalize;
mod registered_buffer;
mod rewrite;
mod value_buffer;

pub(in crate::sql) use accumulator::*;
pub(in crate::sql) use analysis::*;
pub(in crate::sql) use distinct::*;
pub(in crate::sql) use finalize::*;
pub(in crate::sql) use registered_buffer::*;
pub(in crate::sql) use rewrite::*;
pub(in crate::sql) use value_buffer::*;

#[cfg(test)]
mod tests;
