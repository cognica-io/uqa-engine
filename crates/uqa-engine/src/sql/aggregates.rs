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
fn resolve_having(
    engine: &Engine,
    expression: &ScalarExpr,
    _projected_row: &ResultRow,
    statement: &QueryBlockPlan,
    accumulators: &[AggregateAccumulator],
    _group_values: &[Value],
    _params: &[SQLParam],
) -> Result<ScalarExpr, SQLError> {
    fn walk(
        engine: &Engine,
        expression: &ScalarExpr,
        statement: &QueryBlockPlan,
        accumulators: &[AggregateAccumulator],
    ) -> Result<ScalarExpr, SQLError> {
        if is_aggregate(engine, expression) {
            for (index, aggregate) in aggregate_exprs(engine, &statement.projections)
                .into_iter()
                .enumerate()
            {
                if exprs_match(aggregate, expression) {
                    let ScalarExpr::Func { name, args, .. } = aggregate else {
                        return Err(SQLError::Internal(
                            "aggregate classifier returned a non-function expression".into(),
                        ));
                    };
                    let accumulator = accumulators.get(index).ok_or_else(|| {
                        SQLError::Internal("HAVING aggregate accumulator missing".into())
                    })?;
                    return Ok(ScalarExpr::Literal(aggregate_value_with_args(
                        name,
                        accumulator,
                        args,
                    )?));
                }
            }
            return Err(SQLError::Unsupported(
                "HAVING references an aggregate that is not in the SELECT list".into(),
            ));
        }

        Ok(match expression {
            ScalarExpr::Func {
                name,
                args,
                distinct,
                order_by,
                filter,
            } => ScalarExpr::Func {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|argument| walk(engine, argument, statement, accumulators))
                    .collect::<Result<Vec<_>, _>>()?,
                distinct: *distinct,
                order_by: order_by.clone(),
                filter: filter.clone(),
            },
            ScalarExpr::Array(items) => ScalarExpr::Array(
                items
                    .iter()
                    .map(|item| walk(engine, item, statement, accumulators))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            ScalarExpr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
                op: *op,
                lhs: Box::new(walk(engine, lhs, statement, accumulators)?),
                rhs: Box::new(walk(engine, rhs, statement, accumulators)?),
            },
            ScalarExpr::Not(inner) => {
                ScalarExpr::Not(Box::new(walk(engine, inner, statement, accumulators)?))
            }
            ScalarExpr::And(items) => ScalarExpr::And(
                items
                    .iter()
                    .map(|item| walk(engine, item, statement, accumulators))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            ScalarExpr::Or(items) => ScalarExpr::Or(
                items
                    .iter()
                    .map(|item| walk(engine, item, statement, accumulators))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
                expr: Box::new(walk(engine, expr, statement, accumulators)?),
                negated: *negated,
            },
            ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
                expr: Box::new(walk(engine, expr, statement, accumulators)?),
                low: Box::new(walk(engine, low, statement, accumulators)?),
                high: Box::new(walk(engine, high, statement, accumulators)?),
            },
            ScalarExpr::InList {
                expr,
                list,
                negated,
            } => ScalarExpr::InList {
                expr: Box::new(walk(engine, expr, statement, accumulators)?),
                list: list
                    .iter()
                    .map(|item| walk(engine, item, statement, accumulators))
                    .collect::<Result<Vec<_>, _>>()?,
                negated: *negated,
            },
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            } => ScalarExpr::Case {
                base: base
                    .as_deref()
                    .map(|expr| walk(engine, expr, statement, accumulators).map(Box::new))
                    .transpose()?,
                when: when
                    .iter()
                    .map(|(condition, result)| {
                        Ok((
                            walk(engine, condition, statement, accumulators)?,
                            walk(engine, result, statement, accumulators)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, SQLError>>()?,
                else_branch: else_branch
                    .as_deref()
                    .map(|expr| walk(engine, expr, statement, accumulators).map(Box::new))
                    .transpose()?,
            },
            ScalarExpr::Cast { expr, ty } => ScalarExpr::Cast {
                expr: Box::new(walk(engine, expr, statement, accumulators)?),
                ty: ty.clone(),
            },
            ScalarExpr::InSubquery {
                expr,
                subquery,
                negated,
            } => ScalarExpr::InSubquery {
                expr: Box::new(walk(engine, expr, statement, accumulators)?),
                subquery: *subquery,
                negated: *negated,
            },
            other => other.clone(),
        })
    }

    walk(engine, expression, statement, accumulators)
}

fn exprs_match(lhs: &ScalarExpr, rhs: &ScalarExpr) -> bool {
    match (lhs, rhs) {
        (ScalarExpr::Star, ScalarExpr::Star) => true,
        (ScalarExpr::Column(a), ScalarExpr::Column(b)) => a == b,
        (
            ScalarExpr::QualifiedColumn {
                qualifier: aq,
                column: ac,
                ..
            },
            ScalarExpr::QualifiedColumn {
                qualifier: bq,
                column: bc,
                ..
            },
        ) => aq == bq && ac == bc,
        (ScalarExpr::Column(c), ScalarExpr::QualifiedColumn { column, .. })
        | (ScalarExpr::QualifiedColumn { column, .. }, ScalarExpr::Column(c)) => c == column,
        (ScalarExpr::Literal(a), ScalarExpr::Literal(b)) => literals_equal(a, b),
        (ScalarExpr::Param(a), ScalarExpr::Param(b)) => a == b,
        (
            ScalarExpr::Func {
                name: an,
                args: aa,
                distinct: ad,
                order_by: ao,
                filter: af,
            },
            ScalarExpr::Func {
                name: bn,
                args: ba,
                distinct: bd,
                order_by: bo,
                filter: bf,
            },
        ) => {
            an.eq_ignore_ascii_case(bn)
                && ad == bd
                && aa.len() == ba.len()
                && aa.iter().zip(ba.iter()).all(|(x, y)| exprs_match(x, y))
                && ao.len() == bo.len()
                && ao.iter().zip(bo.iter()).all(|(x, y)| {
                    x.descending == y.descending
                        && x.nulls == y.nulls
                        && exprs_match(&x.expr, &y.expr)
                })
                && match (af.as_deref(), bf.as_deref()) {
                    (None, None) => true,
                    (Some(x), Some(y)) => exprs_match(x, y),
                    _ => false,
                }
        }
        (
            ScalarExpr::Binary {
                op: ao,
                lhs: al,
                rhs: ar,
            },
            ScalarExpr::Binary {
                op: bo,
                lhs: bl,
                rhs: br,
            },
        ) => ao == bo && exprs_match(al, bl) && exprs_match(ar, br),
        (ScalarExpr::And(a), ScalarExpr::And(b)) | (ScalarExpr::Or(a), ScalarExpr::Or(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| exprs_match(x, y))
        }
        (ScalarExpr::Not(a), ScalarExpr::Not(b)) => exprs_match(a, b),
        (ScalarExpr::Cast { expr: a, ty: at }, ScalarExpr::Cast { expr: b, ty: bt }) => {
            at == bt && exprs_match(a, b)
        }
        _ => false,
    }
}

fn literals_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bytes(x), Value::Bytes(y)) => x == y,
        (Value::Temporal(x), Value::Temporal(y)) => x == y,
        _ => false,
    }
}

pub(super) fn has_aggregate(engine: &Engine, projections: &[ProjectionPlan]) -> bool {
    projections
        .iter()
        .any(|p| contains_aggregate(engine, &p.expr))
}

fn is_aggregate(engine: &Engine, expr: &ScalarExpr) -> bool {
    matches!(expr, ScalarExpr::Func { name, .. } if matches!(
        name.to_ascii_lowercase().as_str(),
        "count"
            | "sum"
            | "avg"
            | "min"
            | "max"
            | "string_agg"
            | "array_agg"
            | "bool_and"
            | "bool_or"
            | "stddev"
            | "stddev_samp"
            | "stddev_pop"
            | "variance"
            | "var_samp"
            | "var_pop"
            | "percentile_cont"
            | "percentile_disc"
            | "mode"
            | "json_agg"
            | "jsonb_agg"
            | "json_object_agg"
            | "jsonb_object_agg"
    ) || engine.has_registered_aggregate_function(name))
}

fn aggregate_exprs<'a>(engine: &Engine, projections: &'a [ProjectionPlan]) -> Vec<&'a ScalarExpr> {
    let mut out = Vec::new();
    for projection in projections {
        collect_aggregate_exprs(engine, &projection.expr, &mut out);
    }
    out
}

fn collect_aggregate_exprs<'a>(
    engine: &Engine,
    expr: &'a ScalarExpr,
    out: &mut Vec<&'a ScalarExpr>,
) {
    if is_aggregate(engine, expr) {
        out.push(expr);
        return;
    }
    match expr {
        ScalarExpr::Func { args, filter, .. } => {
            for arg in args {
                collect_aggregate_exprs(engine, arg, out);
            }
            if let Some(filter) = filter.as_deref() {
                collect_aggregate_exprs(engine, filter, out);
            }
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            for item in items {
                collect_aggregate_exprs(engine, item, out);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_aggregate_exprs(engine, lhs, out);
            collect_aggregate_exprs(engine, rhs, out);
        }
        ScalarExpr::Not(inner) | ScalarExpr::Cast { expr: inner, .. } => {
            collect_aggregate_exprs(engine, inner, out);
        }
        ScalarExpr::IsNull { expr, .. } => collect_aggregate_exprs(engine, expr, out),
        ScalarExpr::Between { expr, low, high } => {
            collect_aggregate_exprs(engine, expr, out);
            collect_aggregate_exprs(engine, low, out);
            collect_aggregate_exprs(engine, high, out);
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_aggregate_exprs(engine, expr, out);
            for item in list {
                collect_aggregate_exprs(engine, item, out);
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_deref() {
                collect_aggregate_exprs(engine, base, out);
            }
            for (condition, result) in when {
                collect_aggregate_exprs(engine, condition, out);
                collect_aggregate_exprs(engine, result, out);
            }
            if let Some(else_branch) = else_branch.as_deref() {
                collect_aggregate_exprs(engine, else_branch, out);
            }
        }
        ScalarExpr::InSubquery { expr, .. } => collect_aggregate_exprs(engine, expr, out),
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
}

fn contains_aggregate(engine: &Engine, expr: &ScalarExpr) -> bool {
    let mut found = Vec::new();
    collect_aggregate_exprs(engine, expr, &mut found);
    !found.is_empty()
}

/// Collect the top-level column names an expression reads. Returns
/// `false` when the expression can reach arbitrary fields (`*`,
/// subqueries, window calls), in which case callers must materialise
/// whole documents.
fn expr_references_columns(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::Star | ScalarExpr::Column(_) | ScalarExpr::QualifiedColumn { .. } => true,
        ScalarExpr::Func { args, filter, .. } => {
            args.iter().any(expr_references_columns)
                || filter.as_deref().is_some_and(expr_references_columns)
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(expr_references_columns)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_references_columns(lhs) || expr_references_columns(rhs)
        }
        ScalarExpr::Not(inner) | ScalarExpr::Cast { expr: inner, .. } => {
            expr_references_columns(inner)
        }
        ScalarExpr::IsNull { expr, .. } => expr_references_columns(expr),
        ScalarExpr::Between { expr, low, high } => {
            expr_references_columns(expr)
                || expr_references_columns(low)
                || expr_references_columns(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_references_columns(expr) || list.iter().any(expr_references_columns)
        }
        ScalarExpr::WindowCall { args, .. } => args.iter().any(expr_references_columns),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_some_and(expr_references_columns)
                || when.iter().any(|(condition, result)| {
                    expr_references_columns(condition) || expr_references_columns(result)
                })
                || else_branch.as_deref().is_some_and(expr_references_columns)
        }
        ScalarExpr::InSubquery { expr, .. } => expr_references_columns(expr),
        ScalarExpr::ScalarSubquery(_) | ScalarExpr::Exists { .. } => true,
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => false,
    }
}

fn group_context_row(stmt: &QueryBlockPlan, group_values: &[Value]) -> ResultRow {
    let mut row = ResultRow::new();
    for (expr, value) in stmt.group_by.iter().zip(group_values) {
        match expr {
            ScalarExpr::Column(column) => {
                row.insert(column.clone(), value.clone());
            }
            ScalarExpr::QualifiedColumn {
                qualifier,
                column,
                key,
            } => {
                if key.is_empty() {
                    row.insert(format!("{qualifier}.{column}"), value.clone());
                } else {
                    row.insert(key.clone(), value.clone());
                }
                row.insert(column.clone(), value.clone());
            }
            _ => {}
        }
    }
    row
}

fn replace_aggregates_with_values(
    engine: &Engine,
    expr: &ScalarExpr,
    accs: &[AggregateAccumulator],
    cursor: &mut usize,
) -> Result<ScalarExpr, SQLError> {
    if is_aggregate(engine, expr) {
        let ScalarExpr::Func { name, args, .. } = expr else {
            return Err(SQLError::Internal("aggregate expr lost".into()));
        };
        let Some(acc) = accs.get(*cursor) else {
            return Err(SQLError::Internal("aggregate accumulator missing".into()));
        };
        *cursor += 1;
        return Ok(ScalarExpr::Literal(aggregate_value_with_args(
            name, acc, args,
        )?));
    }
    match expr {
        ScalarExpr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => Ok(ScalarExpr::Func {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| replace_aggregates_with_values(engine, arg, accs, cursor))
                .collect::<Result<Vec<_>, _>>()?,
            distinct: *distinct,
            order_by: order_by.clone(),
            filter: filter
                .as_deref()
                .map(|filter| {
                    replace_aggregates_with_values(engine, filter, accs, cursor).map(Box::new)
                })
                .transpose()?,
        }),
        ScalarExpr::Array(items) => Ok(ScalarExpr::Array(
            items
                .iter()
                .map(|item| replace_aggregates_with_values(engine, item, accs, cursor))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::Binary { op, lhs, rhs } => Ok(ScalarExpr::Binary {
            op: *op,
            lhs: Box::new(replace_aggregates_with_values(engine, lhs, accs, cursor)?),
            rhs: Box::new(replace_aggregates_with_values(engine, rhs, accs, cursor)?),
        }),
        ScalarExpr::Not(inner) => Ok(ScalarExpr::Not(Box::new(replace_aggregates_with_values(
            engine, inner, accs, cursor,
        )?))),
        ScalarExpr::And(parts) => Ok(ScalarExpr::And(
            parts
                .iter()
                .map(|part| replace_aggregates_with_values(engine, part, accs, cursor))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::Or(parts) => Ok(ScalarExpr::Or(
            parts
                .iter()
                .map(|part| replace_aggregates_with_values(engine, part, accs, cursor))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::IsNull { expr, negated } => Ok(ScalarExpr::IsNull {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
            negated: *negated,
        }),
        ScalarExpr::Between { expr, low, high } => Ok(ScalarExpr::Between {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
            low: Box::new(replace_aggregates_with_values(engine, low, accs, cursor)?),
            high: Box::new(replace_aggregates_with_values(engine, high, accs, cursor)?),
        }),
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => Ok(ScalarExpr::InList {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
            list: list
                .iter()
                .map(|item| replace_aggregates_with_values(engine, item, accs, cursor))
                .collect::<Result<Vec<_>, _>>()?,
            negated: *negated,
        }),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => Ok(ScalarExpr::Case {
            base: base
                .as_deref()
                .map(|base| {
                    replace_aggregates_with_values(engine, base, accs, cursor).map(Box::new)
                })
                .transpose()?,
            when: when
                .iter()
                .map(|(condition, result)| {
                    Ok((
                        replace_aggregates_with_values(engine, condition, accs, cursor)?,
                        replace_aggregates_with_values(engine, result, accs, cursor)?,
                    ))
                })
                .collect::<Result<Vec<_>, SQLError>>()?,
            else_branch: else_branch
                .as_deref()
                .map(|branch| {
                    replace_aggregates_with_values(engine, branch, accs, cursor).map(Box::new)
                })
                .transpose()?,
        }),
        ScalarExpr::Cast { expr, ty } => Ok(ScalarExpr::Cast {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
            ty: ty.clone(),
        }),
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => Ok(ScalarExpr::InSubquery {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
            subquery: *subquery,
            negated: *negated,
        }),
        other => Ok(other.clone()),
    }
}

fn aggregate_input_value(
    name: &str,
    args: &[ScalarExpr],
    order_by: &[ScalarOrder],
    ctx: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    if name.eq_ignore_ascii_case("count") && (args.is_empty() || matches!(args, [ScalarExpr::Star]))
    {
        return Ok(Value::Int(1));
    }
    // Ordered-set aggregates: the percentile / mode fraction is a
    // direct positional argument; the value to fold comes from
    // `WITHIN GROUP (ORDER BY ...)` which the compiler parks in
    // `order_by[0]`.
    if is_ordered_set_aggregate(name) {
        return order_by
            .first()
            .map(|ob| eval_scalar(&ob.expr, ctx))
            .transpose()
            .map(|v| v.unwrap_or(Value::Null));
    }
    if is_json_object_aggregate(name) {
        return match args {
            [key_expr, value_expr] => {
                let key = eval_scalar(key_expr, ctx)?;
                if matches!(key, Value::Null) {
                    return Ok(Value::Null);
                }
                let value = eval_scalar(value_expr, ctx)?;
                Ok(Value::List(vec![key, value]))
            }
            _ => Err(SQLError::TypeMismatch(format!(
                "{name} requires 2 arguments"
            ))),
        };
    }
    if is_json_array_aggregate(name) {
        return match args {
            [arg] => eval_scalar(arg, ctx),
            _ => Err(SQLError::TypeMismatch(format!(
                "{name} requires 1 argument"
            ))),
        };
    }
    let arg = args
        .first()
        .ok_or_else(|| SQLError::Internal("aggregate missing arg".into()))?;
    eval_scalar(arg, ctx)
}

fn aggregate_input_values(
    args: &[ScalarExpr],
    ctx: &ScalarEvalContext<'_>,
) -> Result<Vec<Value>, SQLError> {
    args.iter()
        .map(|arg| match arg {
            ScalarExpr::Star => Ok(Value::Int(1)),
            other => eval_scalar(other, ctx),
        })
        .collect()
}

fn new_aggregate_accumulators_with_budget(
    engine: &Engine,
    aggregate_targets: &[&ScalarExpr],
    budget_bytes: usize,
) -> Result<Vec<AggregateAccumulator>, SQLError> {
    aggregate_targets
        .iter()
        .map(|expression| match expression {
            ScalarExpr::Func { name, .. } => {
                Ok(engine.registered_aggregate_function(name).map_or_else(
                    || AggregateAccumulator::builtin_with_budget(name, budget_bytes),
                    |function| AggregateAccumulator::registered_with_budget(function, budget_bytes),
                ))
            }
            _ => Ok(AggregateAccumulator::with_budget(budget_bytes)),
        })
        .collect()
}

fn observe_aggregate(
    acc: &mut AggregateAccumulator,
    name: &str,
    args: &[ScalarExpr],
    distinct: bool,
    order_by: &[ScalarOrder],
    ctx: &ScalarEvalContext<'_>,
) -> Result<(), SQLError> {
    if acc.registered.is_some() {
        let values = aggregate_input_values(args, ctx)?;
        if distinct {
            let key = distinct_key(&Value::List(values.clone()))?;
            if !acc.distinct.insert(key)? {
                return Ok(());
            }
        }
        let mut sort_keys: Vec<(Value, bool)> = Vec::with_capacity(order_by.len());
        for ob in order_by {
            let v = eval_scalar(&ob.expr, ctx)?;
            sort_keys.push((v, ob.descending));
        }
        acc.observe_registered(values, sort_keys)?;
        return Ok(());
    }

    let value = aggregate_input_value(name, args, order_by, ctx)?;
    observe_builtin_aggregate_value(acc, name, &value, distinct, order_by, ctx)
}

fn observe_builtin_aggregate_value(
    acc: &mut AggregateAccumulator,
    name: &str,
    value: &Value,
    distinct: bool,
    order_by: &[ScalarOrder],
    ctx: &ScalarEvalContext<'_>,
) -> Result<(), SQLError> {
    let preserves_null_inputs = is_json_array_aggregate(name);
    if distinct && (preserves_null_inputs || !matches!(value, Value::Null)) {
        let key = distinct_key(value)?;
        if !acc.distinct.insert(key)? {
            return Ok(());
        }
    }
    let mut sort_keys: Vec<(Value, bool)> = Vec::with_capacity(order_by.len());
    for ob in order_by {
        let v = eval_scalar(&ob.expr, ctx)?;
        sort_keys.push((v, ob.descending));
    }
    if preserves_null_inputs {
        acc.observe_including_null(value, sort_keys)?;
    } else if order_by.is_empty() {
        acc.observe(value)?;
    } else {
        acc.observe_with_sort_keys(value, sort_keys)?;
    }
    Ok(())
}

fn is_json_array_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("json_agg") || name.eq_ignore_ascii_case("jsonb_agg")
}

fn is_json_object_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("json_object_agg") || name.eq_ignore_ascii_case("jsonb_object_agg")
}

fn is_ordered_set_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("percentile_cont")
        || name.eq_ignore_ascii_case("percentile_disc")
        || name.eq_ignore_ascii_case("mode")
}

pub(super) struct AggregateAccumulator {
    registered: Option<Arc<dyn SQLAggregateFunction>>,
    registered_state: Option<Box<dyn SQLAggregateState>>,
    registered_ordered: RegisteredAggregateBuffer,
    count: u64,
    sum: f64,
    integer_sum: i128,
    decimal_sum: Option<DecimalValue>,
    numeric_inputs: NumericInputKind,
    min: Option<Value>,
    max: Option<Value>,
    /// Distinct-bookkeeping. Filled by the dispatcher when the
    /// aggregate was annotated with `DISTINCT`. Holds canonical-form
    /// keys so `Int(1)` and `Float(1.0)` collapse to the same bucket.
    distinct: DistinctTracker,
    /// Only collection, ordered-set, and statistical aggregates need
    /// their complete input. Streaming aggregates keep constant-size
    /// state and must not spill values that their finalizer never reads.
    state_plan: AggregateStatePlan,
    values: AggregateValueBuffer,
    /// Boolean folds for `BOOL_AND` / `BOOL_OR`. Stay `None` until the
    /// first observation so an empty input set returns `NULL` (matches
    /// `PostgreSQL`).
    bool_and: Option<bool>,
    bool_or: Option<bool>,
    /// Welford state for variance/stddev. This avoids retaining the complete
    /// group for statistical aggregates.
    statistics_count: u64,
    statistics_mean: f64,
    statistics_m2: f64,
    statistics_has_float: bool,
}

#[derive(Clone, Copy, Default)]
enum NumericInputKind {
    #[default]
    Integers,
    Decimals,
    Floats,
    DecimalsAndFloats,
}

impl NumericInputKind {
    fn observe_decimal(&mut self) {
        *self = match self {
            Self::Integers | Self::Decimals => Self::Decimals,
            Self::Floats | Self::DecimalsAndFloats => Self::DecimalsAndFloats,
        };
    }

    fn observe_float(&mut self) {
        *self = match self {
            Self::Integers | Self::Floats => Self::Floats,
            Self::Decimals | Self::DecimalsAndFloats => Self::DecimalsAndFloats,
        };
    }

    fn all_integers(self) -> bool {
        matches!(self, Self::Integers)
    }

    fn decimal_without_float(self) -> bool {
        matches!(self, Self::Decimals)
    }

    fn has_decimal(self) -> bool {
        matches!(self, Self::Decimals | Self::DecimalsAndFloats)
    }
}

#[derive(Clone, Copy)]
enum AggregateStatePlan {
    /// Conservative fallback for an aggregate whose state requirements
    /// are not known here.
    Generic,
    Count,
    Sum,
    Min,
    Max,
    BoolAnd,
    BoolOr,
    Buffered,
    Statistics,
}

impl AggregateStatePlan {
    fn builtin(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "count" => Self::Count,
            "sum" | "avg" => Self::Sum,
            "min" => Self::Min,
            "max" => Self::Max,
            "bool_and" => Self::BoolAnd,
            "bool_or" => Self::BoolOr,
            "stddev" | "stddev_samp" | "stddev_pop" | "variance" | "var_samp" | "var_pop" => {
                Self::Statistics
            }
            "string_agg" | "array_agg" | "json_agg" | "jsonb_agg" | "json_object_agg"
            | "jsonb_object_agg" | "percentile_cont" | "percentile_disc" | "mode" => Self::Buffered,
            _ => Self::Generic,
        }
    }

    fn retains_values(self) -> bool {
        matches!(self, Self::Generic | Self::Buffered)
    }
}

impl Default for AggregateAccumulator {
    fn default() -> Self {
        Self {
            registered: None,
            registered_state: None,
            registered_ordered: RegisteredAggregateBuffer::default(),
            count: 0,
            sum: 0.0,
            integer_sum: 0,
            decimal_sum: None,
            numeric_inputs: NumericInputKind::default(),
            min: None,
            max: None,
            distinct: DistinctTracker::default(),
            state_plan: AggregateStatePlan::Generic,
            values: AggregateValueBuffer::default(),
            bool_and: None,
            bool_or: None,
            statistics_count: 0,
            statistics_mean: 0.0,
            statistics_m2: 0.0,
            statistics_has_float: false,
        }
    }
}

impl AggregateAccumulator {
    fn with_budget(budget_bytes: usize) -> Self {
        let component_budget = (budget_bytes / 2).max(1);
        Self {
            distinct: DistinctTracker::new(component_budget),
            values: AggregateValueBuffer::new(component_budget),
            registered_ordered: RegisteredAggregateBuffer::new(component_budget),
            ..Self::default()
        }
    }

    pub(super) fn builtin(name: &str) -> Self {
        Self {
            state_plan: AggregateStatePlan::builtin(name),
            ..Self::default()
        }
    }

    fn builtin_with_budget(name: &str, budget_bytes: usize) -> Self {
        Self {
            state_plan: AggregateStatePlan::builtin(name),
            ..Self::with_budget(budget_bytes)
        }
    }

    fn registered_with_budget(
        function: Arc<dyn SQLAggregateFunction>,
        budget_bytes: usize,
    ) -> Self {
        let state = function.create_state();
        Self {
            registered: Some(function),
            registered_state: Some(state),
            ..Self::with_budget(budget_bytes)
        }
    }

    pub(super) fn observe(&mut self, value: &Value) -> Result<(), SQLError> {
        if matches!(value, Value::Null) {
            return Ok(());
        }
        self.observe_state(value)?;
        if self.state_plan.retains_values() {
            self.values.push(value.clone(), Vec::new())?;
        }
        Ok(())
    }

    fn observe_state(&mut self, value: &Value) -> Result<(), SQLError> {
        match self.state_plan {
            AggregateStatePlan::Generic => {
                self.count = self
                    .count
                    .checked_add(1)
                    .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
                if matches!(value, Value::Int(_) | Value::Float(_) | Value::Decimal(_)) {
                    self.observe_sum(value)?;
                }
                self.observe_min(value);
                self.observe_max(value);
                if matches!(value, Value::Bool(_)) {
                    self.observe_bool_and(value)?;
                    self.observe_bool_or(value)?;
                }
            }
            AggregateStatePlan::Count => {
                self.count = self
                    .count
                    .checked_add(1)
                    .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
            }
            AggregateStatePlan::Sum => {
                self.count = self
                    .count
                    .checked_add(1)
                    .ok_or_else(|| SQLError::TypeMismatch("aggregate count overflow".into()))?;
                self.observe_sum(value)?;
            }
            AggregateStatePlan::Min => self.observe_min(value),
            AggregateStatePlan::Max => self.observe_max(value),
            AggregateStatePlan::BoolAnd => self.observe_bool_and(value)?,
            AggregateStatePlan::BoolOr => self.observe_bool_or(value)?,
            AggregateStatePlan::Buffered => {}
            AggregateStatePlan::Statistics => {
                self.statistics_has_float |= matches!(value, Value::Float(_));
                let value = value_as_f64(value)?;
                self.statistics_count = self.statistics_count.checked_add(1).ok_or_else(|| {
                    SQLError::TypeMismatch("statistical aggregate count overflow".into())
                })?;
                let count = self.statistics_count as f64;
                let delta = value - self.statistics_mean;
                self.statistics_mean += delta / count;
                let delta_after = value - self.statistics_mean;
                self.statistics_m2 += delta * delta_after;
            }
        }
        Ok(())
    }

    fn observe_sum(&mut self, value: &Value) -> Result<(), SQLError> {
        if !matches!(value, Value::Int(_) | Value::Float(_) | Value::Decimal(_)) {
            return Err(SQLError::TypeMismatch(format!(
                "SUM/AVG requires a numeric value, got {value:?}"
            )));
        }
        if !matches!(value, Value::Int(_)) && self.numeric_inputs.all_integers() {
            // Integer-only SUM/AVG finalizers use `integer_sum` directly.
            // Seed the floating accumulator once, at the first non-integer,
            // instead of converting and adding every integer row twice.
            self.sum = self.integer_sum as f64;
        }
        match value {
            Value::Int(n) => {
                self.integer_sum = self
                    .integer_sum
                    .checked_add(i128::from(*n))
                    .ok_or_else(|| SQLError::TypeMismatch("integer aggregate overflow".into()))?;
                if self.numeric_inputs.has_decimal() {
                    let next = DecimalValue::from_i64(*n);
                    self.decimal_sum = Some(
                        self.decimal_sum
                            .as_ref()
                            .and_then(|sum| sum.checked_add(&next))
                            .ok_or_else(|| {
                                SQLError::TypeMismatch("decimal aggregate overflow".into())
                            })?,
                    );
                }
            }
            Value::Decimal(d) => {
                let next = match &self.decimal_sum {
                    Some(sum) => sum.checked_add(d),
                    None if self.integer_sum == 0 => Some(d.clone()),
                    None => DecimalValue::parse(&self.integer_sum.to_string())
                        .and_then(|sum| sum.checked_add(d)),
                }
                .ok_or_else(|| SQLError::TypeMismatch("decimal aggregate overflow".into()))?;
                self.decimal_sum = Some(next);
                self.numeric_inputs.observe_decimal();
            }
            Value::Float(_) => {
                self.numeric_inputs.observe_float();
            }
            _ => {
                return Err(SQLError::TypeMismatch(format!(
                    "SUM/AVG requires a numeric value, got {value:?}"
                )))
            }
        }
        if !self.numeric_inputs.all_integers() {
            self.sum += value_as_f64(value)?;
        }
        Ok(())
    }

    fn observe_min(&mut self, value: &Value) {
        match &self.min {
            Some(cur) if !value_lt(value, cur) => {}
            _ => self.min = Some(value.clone()),
        }
    }

    fn observe_max(&mut self, value: &Value) {
        match &self.max {
            Some(cur) if !value_gt(value, cur) => {}
            _ => self.max = Some(value.clone()),
        }
    }

    fn observe_bool_and(&mut self, value: &Value) -> Result<(), SQLError> {
        let Value::Bool(value) = value else {
            return Err(SQLError::TypeMismatch(format!(
                "BOOL_AND requires a boolean value, got {value:?}"
            )));
        };
        self.bool_and = Some(self.bool_and.unwrap_or(true) && *value);
        Ok(())
    }

    fn observe_bool_or(&mut self, value: &Value) -> Result<(), SQLError> {
        let Value::Bool(value) = value else {
            return Err(SQLError::TypeMismatch(format!(
                "BOOL_OR requires a boolean value, got {value:?}"
            )));
        };
        self.bool_or = Some(self.bool_or.unwrap_or(false) || *value);
        Ok(())
    }

    fn observe_with_sort_keys(
        &mut self,
        value: &Value,
        keys: Vec<(Value, bool)>,
    ) -> Result<(), SQLError> {
        if matches!(value, Value::Null) {
            return Ok(());
        }
        self.observe_state(value)?;
        if self.state_plan.retains_values() {
            self.values.push(value.clone(), keys)?;
        }
        Ok(())
    }

    fn observe_including_null(
        &mut self,
        value: &Value,
        keys: Vec<(Value, bool)>,
    ) -> Result<(), SQLError> {
        self.values.push(value.clone(), keys)
    }

    fn observe_registered(
        &mut self,
        values: Vec<Value>,
        sort_keys: Vec<(Value, bool)>,
    ) -> Result<(), SQLError> {
        if sort_keys.is_empty() {
            let state = self
                .registered_state
                .as_mut()
                .ok_or_else(|| SQLError::Internal("registered aggregate state missing".into()))?;
            state.observe(&values)?;
            return Ok(());
        }
        self.registered_ordered.push(values, sort_keys)
    }

    fn registered_value(&self) -> Option<Result<Value, SQLError>> {
        let function = self.registered.as_ref()?;
        if self.registered_ordered.is_empty() {
            let state = self
                .registered_state
                .as_ref()
                .ok_or_else(|| SQLError::Internal("registered aggregate state missing".into()));
            return Some(state.and_then(|state| state.finish()));
        }
        Some((|| {
            let mut state = function.create_state();
            self.registered_ordered
                .observe_ordered_into(state.as_mut())?;
            state.finish()
        })())
    }
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
struct AggregateValueRecord {
    value: Value,
    sort_keys: Vec<(Value, bool)>,
    sequence: u64,
}

struct JsonSpillRun {
    file: tempfile::NamedTempFile,
    max_record_bytes: usize,
}

fn write_json_spill_record(
    writer: &mut impl Write,
    value: &impl serde::Serialize,
    description: &str,
) -> Result<usize, SQLError> {
    let payload = serde_json::to_vec(value).map_err(|error| {
        SQLError::Internal(format!("failed to serialize {description}: {error}"))
    })?;
    let record_bytes = payload
        .len()
        .checked_add(1)
        .ok_or_else(|| SQLError::Internal(format!("{description} size overflow")))?;
    writer
        .write_all(&payload)
        .map_err(|error| SQLError::Internal(format!("failed to write {description}: {error}")))?;
    writer
        .write_all(b"\n")
        .map_err(|error| SQLError::Internal(format!("failed to write {description}: {error}")))?;
    Ok(record_bytes)
}

fn read_bounded_json_spill_record<R: BufRead>(
    reader: &mut R,
    max_record_bytes: usize,
    description: &str,
) -> Result<Option<Vec<u8>>, SQLError> {
    let mut record = Vec::new();
    loop {
        let (chunk_len, terminated) = {
            let available = reader.fill_buf().map_err(|error| {
                SQLError::Internal(format!("failed to read {description}: {error}"))
            })?;
            if available.is_empty() {
                if record.is_empty() {
                    return Ok(None);
                }
                return Err(SQLError::Internal(format!(
                    "truncated {description}: missing record delimiter"
                )));
            }
            match available.iter().position(|byte| *byte == b'\n') {
                Some(index) => (index + 1, true),
                None => (available.len(), false),
            }
        };
        let next_len = record
            .len()
            .checked_add(chunk_len)
            .ok_or_else(|| SQLError::Internal(format!("{description} length overflow")))?;
        if next_len > max_record_bytes {
            return Err(SQLError::Internal(format!(
                "{description} exceeds recorded maximum of {max_record_bytes} bytes"
            )));
        }
        record.try_reserve(chunk_len).map_err(|error| {
            SQLError::Internal(format!(
                "unable to allocate {chunk_len} more bytes for {description}: {error}"
            ))
        })?;
        let available = reader.fill_buf().map_err(|error| {
            SQLError::Internal(format!("failed to read {description}: {error}"))
        })?;
        record.extend_from_slice(&available[..chunk_len]);
        reader.consume(chunk_len);
        if terminated {
            let delimiter = record.pop();
            debug_assert_eq!(delimiter, Some(b'\n'));
            return Ok(Some(record));
        }
    }
}

struct AggregateValueBuffer {
    rows: Vec<AggregateValueRecord>,
    runs: Vec<JsonSpillRun>,
    next_sequence: u64,
    budget_bytes: usize,
    memory_bytes: usize,
}

impl Default for AggregateValueBuffer {
    fn default() -> Self {
        Self::new(64 * 1024 * 1024)
    }
}

impl AggregateValueBuffer {
    fn new(budget_bytes: usize) -> Self {
        Self {
            rows: Vec::new(),
            runs: Vec::new(),
            next_sequence: 0,
            budget_bytes: budget_bytes.max(1),
            memory_bytes: 0,
        }
    }

    fn push(&mut self, value: Value, sort_keys: Vec<(Value, bool)>) -> Result<(), SQLError> {
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("aggregate value sequence overflow".into()))?;
        let record = AggregateValueRecord {
            value,
            sort_keys,
            sequence: self.next_sequence,
        };
        let bytes = serde_json::to_vec(&record)
            .map_err(|error| {
                SQLError::Internal(format!("failed to size aggregate value: {error}"))
            })?
            .len()
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("aggregate value size overflow".into()))?;
        if !self.rows.is_empty()
            && self
                .memory_bytes
                .checked_add(bytes)
                .is_none_or(|total| total > self.budget_bytes)
        {
            self.flush_run()?;
        }
        let next_memory_bytes = self
            .memory_bytes
            .checked_add(bytes)
            .ok_or_else(|| SQLError::Internal("aggregate value size overflow".into()))?;
        self.rows.push(record);
        self.memory_bytes = next_memory_bytes;
        self.next_sequence = next_sequence;
        // One value is indivisible. It passes through memory once and is
        // immediately written when its encoding alone exceeds the budget.
        if self.memory_bytes > self.budget_bytes {
            self.flush_run()?;
        }
        Ok(())
    }

    fn ordered_values(&self) -> Result<Vec<Value>, SQLError> {
        let capacity = usize::try_from(self.next_sequence).map_err(|_| {
            SQLError::Internal("aggregate value count exceeds address space".into())
        })?;
        let mut values = Vec::new();
        values.try_reserve_exact(capacity).map_err(|error| {
            SQLError::Internal(format!(
                "unable to allocate aggregate result for {capacity} values: {error}"
            ))
        })?;
        self.for_each_ordered(|record| {
            values.push(record.value);
            Ok(())
        })?;
        Ok(values)
    }

    fn for_each_ordered(
        &self,
        mut visit: impl FnMut(AggregateValueRecord) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let mut memory = self.rows.clone();
        memory.sort_by(compare_aggregate_value_records);
        let mut readers = Vec::with_capacity(self.runs.len() + usize::from(!memory.is_empty()));
        if !memory.is_empty() {
            readers.push(AggregateValueRunReader::memory(memory));
        }
        for run in &self.runs {
            readers.push(AggregateValueRunReader::file(run)?);
        }
        while let Some((index, _)) = readers
            .iter()
            .enumerate()
            .filter_map(|(index, reader)| reader.current().map(|record| (index, record)))
            .min_by(|(_, left), (_, right)| compare_aggregate_value_records(left, right))
        {
            visit(readers[index].take_current()?)?;
        }
        Ok(())
    }

    fn flush_run(&mut self) -> Result<(), SQLError> {
        if self.rows.is_empty() {
            return Ok(());
        }
        self.rows.sort_by(compare_aggregate_value_records);
        let mut run = tempfile::NamedTempFile::new().map_err(|err| {
            SQLError::Internal(format!("failed to create aggregate spill file: {err}"))
        })?;
        let mut max_record_bytes = 0;
        {
            let mut writer = BufWriter::new(run.as_file_mut());
            for row in self.rows.drain(..) {
                let record_bytes =
                    write_json_spill_record(&mut writer, &row, "aggregate spill row")?;
                max_record_bytes = max_record_bytes.max(record_bytes);
            }
            writer.flush().map_err(|err| {
                SQLError::Internal(format!("failed to flush aggregate spill file: {err}"))
            })?;
        }
        run.as_file_mut().seek(SeekFrom::Start(0)).map_err(|err| {
            SQLError::Internal(format!("failed to rewind aggregate spill file: {err}"))
        })?;
        self.runs.push(JsonSpillRun {
            file: run,
            max_record_bytes,
        });
        self.memory_bytes = 0;
        if self.runs.len() >= AGGREGATE_MERGE_FAN_IN {
            let inputs = self
                .runs
                .drain(..AGGREGATE_MERGE_FAN_IN)
                .collect::<Vec<_>>();
            self.runs.push(merge_aggregate_value_runs(inputs)?);
        }
        Ok(())
    }
}

enum AggregateValueRunReader {
    Memory {
        rows: std::vec::IntoIter<AggregateValueRecord>,
        current: Option<AggregateValueRecord>,
    },
    File {
        reader: BufReader<File>,
        current: Option<AggregateValueRecord>,
        max_record_bytes: usize,
    },
}

impl AggregateValueRunReader {
    fn memory(rows: Vec<AggregateValueRecord>) -> Self {
        let mut rows = rows.into_iter();
        let current = rows.next();
        Self::Memory { rows, current }
    }

    fn file(run: &JsonSpillRun) -> Result<Self, SQLError> {
        let file = run.file.reopen().map_err(|error| {
            SQLError::Internal(format!("failed to reopen aggregate spill file: {error}"))
        })?;
        let mut reader = BufReader::new(file);
        let current = read_aggregate_value_record(&mut reader, run.max_record_bytes)?;
        Ok(Self::File {
            reader,
            current,
            max_record_bytes: run.max_record_bytes,
        })
    }

    fn current(&self) -> Option<&AggregateValueRecord> {
        match self {
            Self::Memory { current, .. } | Self::File { current, .. } => current.as_ref(),
        }
    }

    fn take_current(&mut self) -> Result<AggregateValueRecord, SQLError> {
        match self {
            Self::Memory { rows, current } => {
                let record = current
                    .take()
                    .ok_or_else(|| SQLError::Internal("aggregate memory run exhausted".into()))?;
                *current = rows.next();
                Ok(record)
            }
            Self::File {
                reader,
                current,
                max_record_bytes,
            } => {
                let record = current
                    .take()
                    .ok_or_else(|| SQLError::Internal("aggregate spill run exhausted".into()))?;
                *current = read_aggregate_value_record(reader, *max_record_bytes)?;
                Ok(record)
            }
        }
    }
}

fn read_aggregate_value_record(
    reader: &mut BufReader<File>,
    max_record_bytes: usize,
) -> Result<Option<AggregateValueRecord>, SQLError> {
    let Some(record) =
        read_bounded_json_spill_record(reader, max_record_bytes, "aggregate spill row")?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&record).map(Some).map_err(|error| {
        SQLError::Internal(format!(
            "failed to deserialize aggregate spill row: {error}"
        ))
    })
}

fn merge_aggregate_value_runs(runs: Vec<JsonSpillRun>) -> Result<JsonSpillRun, SQLError> {
    let mut readers = runs
        .iter()
        .map(AggregateValueRunReader::file)
        .collect::<Result<Vec<_>, _>>()?;
    let mut output = tempfile::NamedTempFile::new().map_err(|error| {
        SQLError::Internal(format!("failed to create aggregate merge run: {error}"))
    })?;
    let mut max_record_bytes = 0;
    {
        let mut writer = BufWriter::new(output.as_file_mut());
        while let Some((index, _)) = readers
            .iter()
            .enumerate()
            .filter_map(|(index, reader)| reader.current().map(|record| (index, record)))
            .min_by(|(_, left), (_, right)| compare_aggregate_value_records(left, right))
        {
            let record = readers[index].take_current()?;
            let record_bytes =
                write_json_spill_record(&mut writer, &record, "aggregate merge row")?;
            max_record_bytes = max_record_bytes.max(record_bytes);
        }
        writer.flush().map_err(|error| {
            SQLError::Internal(format!("failed to flush aggregate merge run: {error}"))
        })?;
    }
    output
        .as_file_mut()
        .seek(SeekFrom::Start(0))
        .map_err(|error| {
            SQLError::Internal(format!("failed to rewind aggregate merge run: {error}"))
        })?;
    Ok(JsonSpillRun {
        file: output,
        max_record_bytes,
    })
}

fn compare_aggregate_value_records(a: &AggregateValueRecord, b: &AggregateValueRecord) -> Ordering {
    for ((av, ad), (bv, _bd)) in a.sort_keys.iter().zip(b.sort_keys.iter()) {
        let cmp = av.cmp(bv);
        let cmp = if *ad { cmp.reverse() } else { cmp };
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    a.sequence.cmp(&b.sequence)
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
struct RegisteredAggregateRecord {
    values: Vec<Value>,
    sort_keys: Vec<(Value, bool)>,
    sequence: u64,
}

struct RegisteredAggregateBuffer {
    rows: Vec<RegisteredAggregateRecord>,
    runs: Vec<JsonSpillRun>,
    next_sequence: u64,
    budget_bytes: usize,
    memory_bytes: usize,
}

impl Default for RegisteredAggregateBuffer {
    fn default() -> Self {
        Self::new(64 * 1024 * 1024)
    }
}

impl RegisteredAggregateBuffer {
    fn new(budget_bytes: usize) -> Self {
        Self {
            rows: Vec::new(),
            runs: Vec::new(),
            next_sequence: 0,
            budget_bytes: budget_bytes.max(1),
            memory_bytes: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.runs.is_empty()
    }

    fn push(&mut self, values: Vec<Value>, sort_keys: Vec<(Value, bool)>) -> Result<(), SQLError> {
        let next_sequence = self.next_sequence.checked_add(1).ok_or_else(|| {
            SQLError::Internal("registered aggregate value sequence overflow".into())
        })?;
        let record = RegisteredAggregateRecord {
            values,
            sort_keys,
            sequence: self.next_sequence,
        };
        let bytes = serde_json::to_vec(&record)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "failed to size registered aggregate value: {error}"
                ))
            })?
            .len()
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("registered aggregate value size overflow".into()))?;
        if !self.rows.is_empty()
            && self
                .memory_bytes
                .checked_add(bytes)
                .is_none_or(|total| total > self.budget_bytes)
        {
            self.flush_run()?;
        }
        let next_memory_bytes = self
            .memory_bytes
            .checked_add(bytes)
            .ok_or_else(|| SQLError::Internal("registered aggregate value size overflow".into()))?;
        self.rows.push(record);
        self.memory_bytes = next_memory_bytes;
        self.next_sequence = next_sequence;
        if self.memory_bytes > self.budget_bytes {
            self.flush_run()?;
        }
        Ok(())
    }

    fn observe_ordered_into(&self, state: &mut dyn SQLAggregateState) -> Result<(), SQLError> {
        if self.runs.is_empty() {
            let mut rows = self.rows.clone();
            rows.sort_by(compare_registered_aggregate_records);
            for row in rows {
                state.observe(&row.values)?;
            }
            return Ok(());
        }

        let mut rows = self.rows.clone();
        rows.sort_by(compare_registered_aggregate_records);
        let mut readers = Vec::with_capacity(self.runs.len() + usize::from(!rows.is_empty()));
        if !rows.is_empty() {
            readers.push(RegisteredAggregateRunReader::memory(rows));
        }
        for run in &self.runs {
            readers.push(RegisteredAggregateRunReader::file(run)?);
        }

        while let Some((idx, _)) = readers
            .iter()
            .enumerate()
            .filter_map(|(idx, reader)| reader.current().map(|record| (idx, record)))
            .min_by(|(_, a), (_, b)| compare_registered_aggregate_records(a, b))
        {
            let record = readers[idx].take_current()?;
            state.observe(&record.values)?;
        }
        Ok(())
    }

    fn flush_run(&mut self) -> Result<(), SQLError> {
        if self.rows.is_empty() {
            return Ok(());
        }
        self.rows.sort_by(compare_registered_aggregate_records);
        let mut run = tempfile::NamedTempFile::new().map_err(|err| {
            SQLError::Internal(format!(
                "failed to create registered aggregate spill file: {err}"
            ))
        })?;
        let mut max_record_bytes = 0;
        {
            let mut writer = BufWriter::new(run.as_file_mut());
            for row in self.rows.drain(..) {
                let record_bytes =
                    write_json_spill_record(&mut writer, &row, "registered aggregate spill row")?;
                max_record_bytes = max_record_bytes.max(record_bytes);
            }
            writer.flush().map_err(|err| {
                SQLError::Internal(format!(
                    "failed to flush registered aggregate spill file: {err}"
                ))
            })?;
        }
        run.as_file_mut().seek(SeekFrom::Start(0)).map_err(|err| {
            SQLError::Internal(format!(
                "failed to rewind registered aggregate spill file: {err}"
            ))
        })?;
        self.runs.push(JsonSpillRun {
            file: run,
            max_record_bytes,
        });
        self.memory_bytes = 0;
        if self.runs.len() >= AGGREGATE_MERGE_FAN_IN {
            let inputs = self
                .runs
                .drain(..AGGREGATE_MERGE_FAN_IN)
                .collect::<Vec<_>>();
            self.runs.push(merge_registered_aggregate_runs(inputs)?);
        }
        Ok(())
    }
}

enum RegisteredAggregateRunReader {
    Memory {
        rows: std::vec::IntoIter<RegisteredAggregateRecord>,
        current: Option<RegisteredAggregateRecord>,
    },
    File {
        reader: BufReader<File>,
        current: Option<RegisteredAggregateRecord>,
        max_record_bytes: usize,
    },
}

impl RegisteredAggregateRunReader {
    fn memory(rows: Vec<RegisteredAggregateRecord>) -> Self {
        let mut rows = rows.into_iter();
        let current = rows.next();
        Self::Memory { rows, current }
    }

    fn file(run: &JsonSpillRun) -> Result<Self, SQLError> {
        let file = run.file.reopen().map_err(|err| {
            SQLError::Internal(format!(
                "failed to reopen registered aggregate spill file: {err}"
            ))
        })?;
        let mut reader = BufReader::new(file);
        let current = read_registered_aggregate_record(&mut reader, run.max_record_bytes)?;
        Ok(Self::File {
            reader,
            current,
            max_record_bytes: run.max_record_bytes,
        })
    }

    fn current(&self) -> Option<&RegisteredAggregateRecord> {
        match self {
            Self::Memory { current, .. } | Self::File { current, .. } => current.as_ref(),
        }
    }

    fn take_current(&mut self) -> Result<RegisteredAggregateRecord, SQLError> {
        match self {
            Self::Memory { rows, current } => {
                let record = current.take().ok_or_else(|| {
                    SQLError::Internal("registered aggregate memory run exhausted".into())
                })?;
                *current = rows.next();
                Ok(record)
            }
            Self::File {
                reader,
                current,
                max_record_bytes,
            } => {
                let record = current.take().ok_or_else(|| {
                    SQLError::Internal("registered aggregate spill run exhausted".into())
                })?;
                *current = read_registered_aggregate_record(reader, *max_record_bytes)?;
                Ok(record)
            }
        }
    }
}

fn read_registered_aggregate_record(
    reader: &mut BufReader<File>,
    max_record_bytes: usize,
) -> Result<Option<RegisteredAggregateRecord>, SQLError> {
    let Some(record) =
        read_bounded_json_spill_record(reader, max_record_bytes, "registered aggregate spill row")?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&record).map(Some).map_err(|err| {
        SQLError::Internal(format!(
            "failed to deserialize registered aggregate spill row: {err}"
        ))
    })
}

fn merge_registered_aggregate_runs(runs: Vec<JsonSpillRun>) -> Result<JsonSpillRun, SQLError> {
    let mut readers = runs
        .iter()
        .map(RegisteredAggregateRunReader::file)
        .collect::<Result<Vec<_>, _>>()?;
    let mut output = tempfile::NamedTempFile::new().map_err(|error| {
        SQLError::Internal(format!(
            "failed to create registered aggregate merge run: {error}"
        ))
    })?;
    let mut max_record_bytes = 0;
    {
        let mut writer = BufWriter::new(output.as_file_mut());
        while let Some((index, _)) = readers
            .iter()
            .enumerate()
            .filter_map(|(index, reader)| reader.current().map(|record| (index, record)))
            .min_by(|(_, left), (_, right)| compare_registered_aggregate_records(left, right))
        {
            let record = readers[index].take_current()?;
            let record_bytes =
                write_json_spill_record(&mut writer, &record, "registered aggregate merge row")?;
            max_record_bytes = max_record_bytes.max(record_bytes);
        }
        writer.flush().map_err(|error| {
            SQLError::Internal(format!(
                "failed to flush registered aggregate merge run: {error}"
            ))
        })?;
    }
    output
        .as_file_mut()
        .seek(SeekFrom::Start(0))
        .map_err(|error| {
            SQLError::Internal(format!(
                "failed to rewind registered aggregate merge run: {error}"
            ))
        })?;
    Ok(JsonSpillRun {
        file: output,
        max_record_bytes,
    })
}

fn compare_registered_aggregate_records(
    a: &RegisteredAggregateRecord,
    b: &RegisteredAggregateRecord,
) -> Ordering {
    for ((av, ad), (bv, _bd)) in a.sort_keys.iter().zip(b.sort_keys.iter()) {
        let cmp = av.cmp(bv);
        let cmp = if *ad { cmp.reverse() } else { cmp };
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    a.sequence.cmp(&b.sequence)
}

struct DistinctTracker {
    memory: BTreeSet<String>,
    memory_bytes: usize,
    max_memory_record_bytes: usize,
    budget_bytes: usize,
    disk: Option<tempfile::NamedTempFile>,
    max_disk_record_bytes: usize,
}

impl Default for DistinctTracker {
    fn default() -> Self {
        Self::new(32 * 1024 * 1024)
    }
}

impl DistinctTracker {
    fn new(budget_bytes: usize) -> Self {
        Self {
            memory: BTreeSet::new(),
            memory_bytes: 0,
            max_memory_record_bytes: 0,
            budget_bytes: budget_bytes.max(1),
            disk: None,
            max_disk_record_bytes: 0,
        }
    }

    fn insert(&mut self, key: String) -> Result<bool, SQLError> {
        if self.memory.contains(&key) || self.disk_contains(&key)? {
            return Ok(false);
        }
        let encoded_bytes = serde_json::to_vec(&key)
            .map_err(|error| {
                SQLError::Internal(format!("failed to size aggregate DISTINCT key: {error}"))
            })?
            .len()
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("aggregate DISTINCT size overflow".into()))?;
        self.memory_bytes = self
            .memory_bytes
            .checked_add(encoded_bytes)
            .ok_or_else(|| SQLError::Internal("aggregate DISTINCT size overflow".into()))?;
        self.max_memory_record_bytes = self.max_memory_record_bytes.max(encoded_bytes);
        self.memory.insert(key);
        if self.memory_bytes > self.budget_bytes {
            self.spill()?;
        }
        Ok(true)
    }

    fn disk_contains(&self, wanted: &str) -> Result<bool, SQLError> {
        let Some(file) = self.disk.as_ref() else {
            return Ok(false);
        };
        let file = file.reopen().map_err(|error| {
            SQLError::Internal(format!(
                "failed to reopen aggregate DISTINCT spill: {error}"
            ))
        })?;
        let mut reader = BufReader::new(file);
        loop {
            let Some(record) = read_bounded_json_spill_record(
                &mut reader,
                self.max_disk_record_bytes,
                "aggregate DISTINCT spill row",
            )?
            else {
                return Ok(false);
            };
            let key: String = serde_json::from_slice(&record).map_err(|error| {
                SQLError::Internal(format!(
                    "failed to decode aggregate DISTINCT spill: {error}"
                ))
            })?;
            if key == wanted {
                return Ok(true);
            }
        }
    }

    fn spill(&mut self) -> Result<(), SQLError> {
        if self.memory.is_empty() {
            return Ok(());
        }
        if self.disk.is_none() {
            self.disk = Some(tempfile::NamedTempFile::new().map_err(|error| {
                SQLError::Internal(format!(
                    "failed to create aggregate DISTINCT spill: {error}"
                ))
            })?);
        }
        let file = self.disk.as_mut().ok_or_else(|| {
            SQLError::Internal("aggregate DISTINCT spill file was not initialized".into())
        })?;
        let original_length = file.as_file_mut().seek(SeekFrom::End(0)).map_err(|error| {
            SQLError::Internal(format!("failed to seek aggregate DISTINCT spill: {error}"))
        })?;
        let next_max_disk_record_bytes =
            self.max_disk_record_bytes.max(self.max_memory_record_bytes);
        let result = {
            let mut writer = BufWriter::new(file.as_file_mut());
            let result = (|| -> Result<(), SQLError> {
                for key in &self.memory {
                    serde_json::to_writer(&mut writer, key).map_err(|error| {
                        SQLError::Internal(format!(
                            "failed to encode aggregate DISTINCT key: {error}"
                        ))
                    })?;
                    writer.write_all(b"\n").map_err(|error| {
                        SQLError::Internal(format!(
                            "failed to write aggregate DISTINCT key: {error}"
                        ))
                    })?;
                }
                writer.flush().map_err(|error| {
                    SQLError::Internal(format!("failed to flush aggregate DISTINCT spill: {error}"))
                })
            })();
            drop(writer);
            result
        };
        if let Err(error) = result {
            file.as_file_mut()
                .set_len(original_length)
                .map_err(|rollback| {
                    SQLError::Internal(format!(
                        "{error}; failed to roll back aggregate DISTINCT spill: {rollback}"
                    ))
                })?;
            return Err(error);
        }
        self.memory.clear();
        self.memory_bytes = 0;
        self.max_memory_record_bytes = 0;
        self.max_disk_record_bytes = next_max_disk_record_bytes;
        Ok(())
    }
}

fn distinct_key(v: &Value) -> Result<String, SQLError> {
    Ok(match v {
        Value::Null => "\x00".into(),
        Value::Bool(b) => format!("b:{b}"),
        Value::Int(n) => format!("i:{n}"),
        Value::Float(f) => format!("f:{:016x}", f.to_bits()),
        Value::Decimal(d) => format!("n:{}", d.to_canonical_string()),
        Value::Str(s) => format!("s:{s}"),
        Value::Bytes(bytes) => {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let capacity = bytes
                .len()
                .checked_mul(2)
                .and_then(|length| length.checked_add(2))
                .ok_or_else(|| SQLError::Internal("aggregate DISTINCT key size overflow".into()))?;
            let mut key = String::new();
            key.try_reserve_exact(capacity).map_err(|error| {
                SQLError::Internal(format!(
                    "unable to allocate aggregate DISTINCT key of {capacity} bytes: {error}"
                ))
            })?;
            key.push_str("y:");
            for byte in bytes {
                key.push(char::from(HEX[usize::from(byte >> 4)]));
                key.push(char::from(HEX[usize::from(byte & 0x0f)]));
            }
            key
        }
        Value::Temporal(t) => format!("t:{}", t.to_sql_string()),
        other => format!("o:{other:?}"),
    })
}

fn value_as_f64(v: &Value) -> Result<f64, SQLError> {
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(*f),
        Value::Decimal(d) => d.to_f64().ok_or_else(|| {
            SQLError::TypeMismatch(format!("expected number that fits float, got {v:?}"))
        }),
        other => Err(SQLError::TypeMismatch(format!(
            "expected number, got {other:?}"
        ))),
    }
}

fn value_lt(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x < y,
        (Value::Float(x), Value::Float(y)) => x < y,
        (Value::Int(x), Value::Float(y)) => (*x as f64) < *y,
        (Value::Float(x), Value::Int(y)) => *x < (*y as f64),
        (Value::Decimal(x), Value::Decimal(y)) => x < y,
        (Value::Int(x), Value::Decimal(y)) => DecimalValue::from_i64(*x) < *y,
        (Value::Decimal(x), Value::Int(y)) => *x < DecimalValue::from_i64(*y),
        (Value::Float(x), Value::Decimal(y)) => {
            DecimalValue::from_f64_lossy(*x).is_some_and(|x| x < *y)
        }
        (Value::Decimal(x), Value::Float(y)) => {
            DecimalValue::from_f64_lossy(*y).is_some_and(|y| *x < y)
        }
        (Value::Str(x), Value::Str(y)) => x < y,
        (Value::Temporal(x), Value::Temporal(y)) => x < y,
        _ => false,
    }
}

fn value_gt(a: &Value, b: &Value) -> bool {
    value_lt(b, a)
}

pub(super) fn aggregate_value(name: &str, acc: &AggregateAccumulator) -> Result<Value, SQLError> {
    aggregate_value_with_args(name, acc, &[])
}

fn aggregate_value_with_args(
    name: &str,
    acc: &AggregateAccumulator,
    args: &[ScalarExpr],
) -> Result<Value, SQLError> {
    if let Some(value) = acc.registered_value() {
        return value;
    }
    let lname = name.to_ascii_lowercase();

    let value = match lname.as_str() {
        "count" => Value::Int(
            i64::try_from(acc.count)
                .map_err(|_| SQLError::TypeMismatch("aggregate count exceeds BIGINT".into()))?,
        ),
        "sum" => {
            if acc.count == 0 {
                Value::Null
            } else if acc.numeric_inputs.decimal_without_float() {
                acc.decimal_sum.clone().map_or(Value::Null, Value::Decimal)
            } else if acc.numeric_inputs.all_integers() {
                Value::Int(i64::try_from(acc.integer_sum).map_err(|_| {
                    SQLError::TypeMismatch("integer aggregate result exceeds BIGINT".into())
                })?)
            } else {
                Value::Float(acc.sum)
            }
        }
        "avg" => {
            if acc.count == 0 {
                Value::Null
            } else if acc.numeric_inputs.decimal_without_float() {
                let divisor = DecimalValue::from_i64(i64::try_from(acc.count).map_err(|_| {
                    SQLError::TypeMismatch("aggregate count exceeds BIGINT".into())
                })?);
                let average = acc
                    .decimal_sum
                    .as_ref()
                    .and_then(|sum| sum.checked_div(&divisor))
                    .ok_or_else(|| SQLError::TypeMismatch("decimal AVG overflow".into()))?;
                Value::Decimal(average)
            } else if acc.numeric_inputs.all_integers() {
                Value::Float(acc.integer_sum as f64 / acc.count as f64)
            } else {
                Value::Float(acc.sum / acc.count as f64)
            }
        }
        "min" => acc.min.clone().unwrap_or(Value::Null),
        "max" => acc.max.clone().unwrap_or(Value::Null),
        "string_agg" => {
            let ordered_values = acc.values.ordered_values()?;
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            // Separator: literal second positional arg, or empty.
            let sep = match args.get(1) {
                Some(ScalarExpr::Literal(Value::Str(s))) => s.clone(),
                _ => String::new(),
            };
            let parts: Vec<String> = ordered_values
                .iter()
                .map(|v| match v {
                    Value::Null => Ok(None),
                    Value::Str(s) => Ok(Some(s.clone())),
                    Value::Int(n) => Ok(Some(n.to_string())),
                    Value::Float(f) => Ok(Some(f.to_string())),
                    Value::Decimal(d) => Ok(Some(d.to_sql_string())),
                    Value::Bool(b) => Ok(Some(b.to_string())),
                    Value::Temporal(t) => Ok(Some(t.to_sql_string())),
                    Value::Bytes(_) | Value::List(_) | Value::Map(_) => {
                        Err(SQLError::TypeMismatch(format!(
                            "string_agg requires a text-coercible value, got {v:?}"
                        )))
                    }
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect();
            Value::Str(parts.join(&sep))
        }
        "array_agg" => {
            let ordered_values = acc.values.ordered_values()?;
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            Value::List(ordered_values)
        }
        "json_agg" | "jsonb_agg" => {
            let ordered_values = acc.values.ordered_values()?;
            if ordered_values.is_empty() {
                return Ok(Value::Null);
            }
            Value::List(ordered_values)
        }
        "json_object_agg" | "jsonb_object_agg" => {
            let ordered_values = acc.values.ordered_values()?;
            let mut map = BTreeMap::new();
            for value in ordered_values {
                let Value::List(pair) = value else {
                    return Err(SQLError::Internal(
                        "JSON object aggregate retained a non-pair value".into(),
                    ));
                };
                if pair.len() != 2 {
                    return Err(SQLError::Internal(
                        "JSON object aggregate retained a malformed pair".into(),
                    ));
                }
                if matches!(pair[0], Value::Null) {
                    return Err(SQLError::TypeMismatch(
                        "JSON object aggregate key must not be NULL".into(),
                    ));
                }
                map.insert(aggregate_json_key(&pair[0]), pair[1].clone());
            }
            if map.is_empty() {
                Value::Null
            } else {
                Value::Map(map)
            }
        }
        "bool_and" => match acc.bool_and {
            Some(b) => Value::Bool(b),
            None => Value::Null,
        },
        "bool_or" => match acc.bool_or {
            Some(b) => Value::Bool(b),
            None => Value::Null,
        },
        "stddev" | "stddev_samp" => {
            if acc.statistics_count < 2 {
                return Ok(Value::Null);
            }
            statistical_value(
                acc,
                (acc.statistics_m2 / (acc.statistics_count as f64 - 1.0)).sqrt(),
            )
        }
        "stddev_pop" => {
            if acc.statistics_count == 0 {
                return Ok(Value::Null);
            }
            statistical_value(
                acc,
                (acc.statistics_m2 / acc.statistics_count as f64).sqrt(),
            )
        }
        "variance" | "var_samp" => {
            if acc.statistics_count < 2 {
                return Ok(Value::Null);
            }
            statistical_value(acc, acc.statistics_m2 / (acc.statistics_count as f64 - 1.0))
        }
        "var_pop" => {
            if acc.statistics_count == 0 {
                return Ok(Value::Null);
            }
            statistical_value(acc, acc.statistics_m2 / acc.statistics_count as f64)
        }
        "percentile_cont" => {
            let frac = percentile_fraction(args)?;
            percentile_cont(&acc.values, frac)?.map_or(Value::Null, Value::Float)
        }
        "percentile_disc" => {
            let frac = percentile_fraction(args)?;
            percentile_disc(&acc.values, frac)?.unwrap_or(Value::Null)
        }
        "mode" => mode_value(&acc.values)?,
        _ => return Err(SQLError::UnknownFunction(format!("aggregate `{name}`"))),
    };
    Ok(value)
}

fn percentile_fraction(args: &[ScalarExpr]) -> Result<f64, SQLError> {
    let fraction = match args.first() {
        Some(ScalarExpr::Literal(Value::Float(f))) => *f,
        Some(ScalarExpr::Literal(Value::Int(n))) => *n as f64,
        Some(ScalarExpr::Literal(Value::Decimal(d))) => d.to_f64().ok_or_else(|| {
            SQLError::TypeMismatch("percentile fraction is outside floating-point range".into())
        })?,
        Some(value) => {
            return Err(SQLError::TypeMismatch(format!(
                "percentile fraction must be a numeric literal, got {value:?}"
            )))
        }
        None => {
            return Err(SQLError::BadArity {
                name: "percentile".into(),
                expected: "fraction argument".into(),
                actual: 0,
            })
        }
    };
    if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
        return Err(SQLError::TypeMismatch(format!(
            "percentile fraction must be between 0 and 1, got {fraction}"
        )));
    }
    Ok(fraction)
}

fn aggregate_json_key(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Decimal(d) => d.to_sql_string(),
        Value::Str(s) => s.clone(),
        Value::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::Temporal(t) => t.to_sql_string(),
        Value::List(_) | Value::Map(_) => serde_json::to_string(&core_value_to_json(value))
            .unwrap_or_else(|_| format!("{value:?}")),
    }
}

/// Statistical aggregates (`variance`, `stddev_*`) return `numeric`
/// for integer / numeric inputs in `PostgreSQL` (rendering with a
/// decimal point, e.g. `1.00000...`), and `double precision` only for
/// float inputs.
fn statistical_value(accumulator: &AggregateAccumulator, computed: f64) -> Value {
    if accumulator.statistics_has_float || !computed.is_finite() {
        return Value::Float(computed);
    }
    uqa_core::DecimalValue::parse(&format!("{computed:.16}"))
        .map_or(Value::Float(computed), Value::Decimal)
}

fn percentile_cont(values: &AggregateValueBuffer, frac: f64) -> Result<Option<f64>, SQLError> {
    if values.next_sequence == 0 {
        return Ok(None);
    }
    let position = frac * (values.next_sequence as f64 - 1.0);
    let low = position.floor() as u64;
    let high = position.ceil() as u64;
    let mut low_value = None;
    let mut high_value = None;
    let mut index = 0_u64;
    values.for_each_ordered(|record| {
        if index == low {
            low_value = Some(value_as_f64(&record.value)?);
        }
        if index == high {
            high_value = Some(value_as_f64(&record.value)?);
        }
        index = index
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("percentile aggregate index overflow".into()))?;
        Ok(())
    })?;
    let low_value = low_value.ok_or_else(|| {
        SQLError::Internal("percentile lower value missing from aggregate spill".into())
    })?;
    let high_value = high_value.ok_or_else(|| {
        SQLError::Internal("percentile upper value missing from aggregate spill".into())
    })?;
    let weight = position - low as f64;
    Ok(Some(low_value * (1.0 - weight) + high_value * weight))
}

fn percentile_disc(values: &AggregateValueBuffer, frac: f64) -> Result<Option<Value>, SQLError> {
    if values.next_sequence == 0 {
        return Ok(None);
    }
    let rank = ((frac * values.next_sequence as f64).ceil() as u64)
        .max(1)
        .min(values.next_sequence);
    let mut value = None;
    let mut index = 0_u64;
    values.for_each_ordered(|record| {
        index = index
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("percentile aggregate index overflow".into()))?;
        if index == rank {
            value = Some(record.value);
        }
        Ok(())
    })?;
    Ok(value)
}

fn mode_value(values: &AggregateValueBuffer) -> Result<Value, SQLError> {
    if values.next_sequence == 0 {
        return Ok(Value::Null);
    }
    let mut current_key = None;
    let mut current_value = Value::Null;
    let mut current_count = 0_u64;
    let mut best_value = Value::Null;
    let mut best_count = 0_u64;
    values.for_each_ordered(|record| {
        let key = distinct_key(&record.value)?;
        if current_key.as_ref().is_some_and(|current| current != &key) {
            if current_count >= best_count {
                best_count = current_count;
                best_value = current_value.clone();
            }
            current_count = 0;
        }
        if current_key.as_ref() != Some(&key) {
            current_key = Some(key);
            current_value = record.value;
        }
        current_count = current_count
            .checked_add(1)
            .ok_or_else(|| SQLError::Internal("mode aggregate count overflow".into()))?;
        Ok(())
    })?;
    if current_count >= best_count {
        best_value = current_value;
    }
    Ok(best_value)
}

/// Compute a projection's output column name. `PostgreSQL` reports
/// standalone expressions as `?column?`; `projection_columns` adds a
/// suffix when the row map needs unique keys.
pub(super) fn projection_label_at(proj: &ProjectionPlan) -> String {
    if let Some(a) = &proj.alias {
        return a.clone();
    }
    match &proj.expr {
        ScalarExpr::Column(c) => c.clone(),
        ScalarExpr::QualifiedColumn { column, .. } => column.clone(),
        ScalarExpr::Star => "*".into(),
        ScalarExpr::Func { name, .. } => name.clone(),
        _ => "?column?".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_spill_record_reader_rejects_oversized_and_truncated_records() {
        let mut oversized = std::io::Cursor::new(b"12345\n".to_vec());
        let error = read_bounded_json_spill_record(&mut oversized, 5, "test aggregate spill row")
            .unwrap_err();
        assert!(error.to_string().contains("exceeds recorded maximum"));

        let mut truncated = std::io::Cursor::new(b"12345".to_vec());
        let error = read_bounded_json_spill_record(&mut truncated, 6, "test aggregate spill row")
            .unwrap_err();
        assert!(error.to_string().contains("missing record delimiter"));
    }

    #[test]
    fn streaming_aggregate_does_not_retain_or_spill_inputs() {
        let mut accumulator = AggregateAccumulator::builtin("sum");
        let end = 4097_i64;
        for value in 0..end {
            accumulator.observe(&Value::Int(value)).unwrap();
        }

        assert!(accumulator.values.rows.is_empty());
        assert!(accumulator.values.runs.is_empty());
        assert_eq!(
            aggregate_value("sum", &accumulator).unwrap(),
            Value::Int(end * (end - 1) / 2)
        );
    }

    #[test]
    fn collection_aggregate_still_retains_inputs() {
        let mut accumulator = AggregateAccumulator::builtin("array_agg");
        accumulator.observe(&Value::Int(7)).unwrap();

        assert_eq!(accumulator.count, 0);
        assert_eq!(accumulator.decimal_sum, None);
        assert_eq!(accumulator.min, None);
        assert_eq!(accumulator.max, None);
        assert_eq!(accumulator.values.rows.len(), 1);
        assert_eq!(
            aggregate_value("array_agg", &accumulator).unwrap(),
            Value::List(vec![Value::Int(7)])
        );
    }

    #[test]
    fn ordered_aggregate_buffers_reject_sequence_overflow_without_appending() {
        let mut builtin = AggregateValueBuffer::new(1024);
        builtin.next_sequence = u64::MAX;
        let error = builtin.push(Value::Int(1), Vec::new()).unwrap_err();
        assert!(error.to_string().contains("sequence overflow"));
        assert!(builtin.rows.is_empty());

        let mut registered = RegisteredAggregateBuffer::new(1024);
        registered.next_sequence = u64::MAX;
        let error = registered
            .push(vec![Value::Int(1)], Vec::new())
            .unwrap_err();
        assert!(error.to_string().contains("sequence overflow"));
        assert!(registered.rows.is_empty());
    }

    #[test]
    fn streaming_aggregate_counts_report_overflow() {
        let mut count = AggregateAccumulator::builtin("count");
        count.count = u64::MAX;
        let error = count.observe(&Value::Int(1)).unwrap_err();
        assert!(error.to_string().contains("count overflow"));

        let mut statistics = AggregateAccumulator::builtin("stddev_pop");
        statistics.statistics_count = u64::MAX;
        let error = statistics.observe(&Value::Int(1)).unwrap_err();
        assert!(error.to_string().contains("count overflow"));
    }

    #[test]
    fn tiny_budget_collection_aggregate_spills_and_merge_streams_exact_order() {
        let mut accumulator = AggregateAccumulator::builtin_with_budget("array_agg", 2);
        for value in (0..512_i64).rev() {
            accumulator
                .observe_with_sort_keys(&Value::Int(value), vec![(Value::Int(value), false)])
                .unwrap();
        }

        assert!(!accumulator.values.runs.is_empty());
        assert!(accumulator.values.runs.len() < AGGREGATE_MERGE_FAN_IN);
        assert!(accumulator.values.memory_bytes <= accumulator.values.budget_bytes);
        let expected = Value::List((0..512_i64).map(Value::Int).collect());
        assert_eq!(
            aggregate_value("array_agg", &accumulator).unwrap(),
            expected
        );
    }

    #[test]
    fn collection_aggregate_rejects_a_spill_record_larger_than_writer_metadata() {
        let mut values = AggregateValueBuffer::new(1);
        values
            .push(Value::Int(1), vec![(Value::Int(1), false)])
            .unwrap();
        let run = values.runs.first_mut().unwrap();
        run.file.as_file_mut().seek(SeekFrom::End(0)).unwrap();
        run.file
            .as_file_mut()
            .write_all(&vec![b'x'; run.max_record_bytes])
            .unwrap();
        run.file.as_file_mut().write_all(b"\n").unwrap();
        run.file.as_file_mut().flush().unwrap();

        let error = values.ordered_values().unwrap_err();
        assert!(error.to_string().contains("exceeds recorded maximum"));
    }

    #[test]
    fn tiny_budget_distinct_tracker_migrates_to_disk() {
        let mut tracker = DistinctTracker::new(1);
        assert!(tracker.insert("alpha".into()).unwrap());
        assert!(tracker.disk.is_some());
        assert!(tracker.memory.is_empty());
        assert!(!tracker.insert("alpha".into()).unwrap());
        assert!(tracker.insert("beta".into()).unwrap());
    }

    #[test]
    fn distinct_tracker_rejects_a_spill_record_larger_than_writer_metadata() {
        let mut tracker = DistinctTracker::new(1);
        assert!(tracker.insert("alpha".into()).unwrap());
        let file = tracker.disk.as_mut().unwrap().as_file_mut();
        file.seek(SeekFrom::End(0)).unwrap();
        file.write_all(&vec![b'x'; tracker.max_disk_record_bytes])
            .unwrap();
        file.write_all(b"\n").unwrap();
        file.flush().unwrap();

        let error = tracker.insert("missing".into()).unwrap_err();
        assert!(error.to_string().contains("exceeds recorded maximum"));
    }

    #[test]
    fn builtins_only_update_state_used_by_their_finalizer() {
        let mut count = AggregateAccumulator::builtin("count");
        count.observe(&Value::Int(7)).unwrap();
        assert_eq!(count.count, 1);
        assert_eq!(count.decimal_sum, None);
        assert_eq!(count.min, None);
        assert_eq!(count.max, None);

        let mut sum = AggregateAccumulator::builtin("sum");
        sum.observe(&Value::Int(7)).unwrap();
        assert_eq!(sum.count, 1);
        assert_eq!(sum.integer_sum, 7);
        assert_eq!(sum.decimal_sum, None);
        assert_eq!(sum.min, None);
        assert_eq!(sum.max, None);

        let mut min = AggregateAccumulator::builtin("min");
        min.observe(&Value::Int(7)).unwrap();
        assert_eq!(min.count, 0);
        assert_eq!(min.decimal_sum, None);
        assert_eq!(min.min, Some(Value::Int(7)));
        assert_eq!(min.max, None);

        let mut bool_or = AggregateAccumulator::builtin("bool_or");
        bool_or.observe(&Value::Bool(true)).unwrap();
        assert_eq!(bool_or.count, 0);
        assert_eq!(bool_or.bool_and, None);
        assert_eq!(bool_or.bool_or, Some(true));
    }

    #[test]
    fn statistical_aggregate_uses_constant_welford_state() {
        let mut accumulator = AggregateAccumulator::builtin("stddev_pop");
        accumulator.observe(&Value::Int(7)).unwrap();

        assert_eq!(accumulator.statistics_count, 1);
        assert_eq!(accumulator.decimal_sum, None);
        assert_eq!(accumulator.min, None);
        assert_eq!(accumulator.max, None);
        assert!(accumulator.values.rows.is_empty());
        assert!(accumulator.values.runs.is_empty());
    }

    #[test]
    fn integer_sum_stays_exact_beyond_float_precision() {
        let mut accumulator = AggregateAccumulator::builtin("sum");
        accumulator
            .observe(&Value::Int(9_007_199_254_740_992))
            .unwrap();
        accumulator.observe(&Value::Int(1)).unwrap();

        assert_eq!(
            aggregate_value("sum", &accumulator).unwrap(),
            Value::Int(9_007_199_254_740_993)
        );
        assert_eq!(accumulator.decimal_sum, None);
    }

    #[test]
    fn integer_average_promotes_to_float_only_when_finalized_or_mixed() {
        let mut integers = AggregateAccumulator::builtin("avg");
        integers.observe(&Value::Int(2)).unwrap();
        integers.observe(&Value::Int(3)).unwrap();
        assert_eq!(integers.sum, 0.0);
        assert_eq!(
            aggregate_value("avg", &integers).unwrap(),
            Value::Float(2.5)
        );

        integers.observe(&Value::Float(1.5)).unwrap();
        assert_eq!(integers.sum, 6.5);
        assert_eq!(
            aggregate_value("avg", &integers).unwrap(),
            Value::Float(6.5 / 3.0)
        );
    }

    #[test]
    fn decimal_sum_absorbs_integers_observed_before_and_after_it() {
        let mut accumulator = AggregateAccumulator::builtin("sum");
        accumulator.observe(&Value::Int(2)).unwrap();
        accumulator
            .observe(&Value::Decimal(DecimalValue::parse("0.5").unwrap()))
            .unwrap();
        accumulator.observe(&Value::Int(3)).unwrap();

        assert_eq!(
            aggregate_value("sum", &accumulator).unwrap(),
            Value::Decimal(DecimalValue::parse("5.5").unwrap())
        );
    }

    #[test]
    fn aggregate_finalizers_report_integer_width_overflow() {
        let mut count = AggregateAccumulator::builtin("count");
        count.count = i64::MAX as u64 + 1;
        assert!(aggregate_value("count", &count)
            .unwrap_err()
            .to_string()
            .contains("exceeds BIGINT"));

        let mut sum = AggregateAccumulator::builtin("sum");
        sum.count = 1;
        sum.integer_sum = i128::from(i64::MAX) + 1;
        assert!(aggregate_value("sum", &sum)
            .unwrap_err()
            .to_string()
            .contains("exceeds BIGINT"));
    }

    #[test]
    fn percentile_fraction_rejects_missing_and_out_of_range_values() {
        assert!(percentile_fraction(&[]).is_err());
        for fraction in [-0.1, 1.1, f64::NAN] {
            assert!(percentile_fraction(&[ScalarExpr::Literal(Value::Float(fraction))]).is_err());
        }
        assert_eq!(
            percentile_fraction(&[ScalarExpr::Literal(Value::Float(0.25))]).unwrap(),
            0.25
        );
    }
}
