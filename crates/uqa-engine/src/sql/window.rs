//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL window function evaluation.

use super::scalar::PlanSubqueryArena;
use super::{
    aggregate_value, projection_columns, AggregateAccumulator, CteScope, Engine, SQLError,
    SQLParam, ScopedEngineHook, Value,
};
use uqa_execution::{
    eval_scalar, Batch, ExecResult, ExternalSort, IndexedSpill, PhysicalOperator, PhysicalRow,
    RowSchema, ScalarEvalContext, ScalarExpr, ScalarFrameBound, ScalarOrder, ScalarSubqueryRunner,
    ScalarWindowSpec, SortKey, SpillBuffer, SpillScan, WindowExecutor,
};
use uqa_planner::ProjectionPlan;

mod planning;

pub(in crate::sql) use planning::expr_has_window;
use planning::rewrite_window_expr;

#[derive(Clone)]
struct WindowSlot {
    column: uqa_sql::ast::InternalColumnRef,
    name: String,
    args: Vec<ScalarExpr>,
    spec: ScalarWindowSpec,
}

pub(super) struct PreparedWindowPlan {
    slots: Vec<WindowSlot>,
    projections: Vec<ProjectionPlan>,
}

impl PreparedWindowPlan {
    pub(super) fn projections(&self) -> &[ProjectionPlan] {
        &self.projections
    }

    pub(super) fn output_schema(
        &self,
        engine: &Engine,
        input: &RowSchema,
        params: &[SQLParam],
    ) -> Result<RowSchema, SQLError> {
        let mut schema = input.clone();
        for slot in &self.slots {
            if schema.internal_slot(slot.column).is_some() {
                continue;
            }
            let expression = ScalarExpr::WindowCall {
                name: slot.name.clone(),
                args: slot.args.clone(),
                spec: slot.spec.clone(),
            };
            let ty =
                uqa_execution::scalar_type_with_resolver(&expression, &schema, params, engine)?;
            schema = RowSchema::append_internal_typed(&schema, &[(slot.column, ty)]);
        }
        Ok(schema)
    }
}

pub(super) struct PhysicalWindowExecutor<'a> {
    engine: &'a Engine,
    plan: PreparedWindowPlan,
    params: &'a [SQLParam],
    ctes: CteScope,
    schema: RowSchema,
    work_mem_bytes: usize,
    input: Option<SpillBuffer>,
}

impl<'a> PhysicalWindowExecutor<'a> {
    pub(super) fn new(
        engine: &'a Engine,
        plan: PreparedWindowPlan,
        params: &'a [SQLParam],
        ctes: &CteScope,
        schema: RowSchema,
        work_mem_bytes: usize,
    ) -> Self {
        Self {
            engine,
            plan,
            params,
            ctes: ctes.clone(),
            schema,
            work_mem_bytes,
            input: Some(SpillBuffer::new((work_mem_bytes / 3).max(1))),
        }
    }
}

impl WindowExecutor for PhysicalWindowExecutor<'_> {
    fn consume(&mut self, batch: Batch) -> ExecResult<()> {
        self.input
            .as_mut()
            .ok_or_else(|| {
                uqa_execution::ExecError::Other("window executor already finalized".into())
            })?
            .push(batch)?;
        Ok(())
    }

    fn finish(&mut self) -> ExecResult<SpillBuffer> {
        let input = self.input.take().ok_or_else(|| {
            uqa_execution::ExecError::Other("window executor already finalized".into())
        })?;
        Ok(execute_window_plan(
            self.engine,
            &self.plan,
            input,
            &self.schema,
            self.work_mem_bytes,
            self.params,
            &self.ctes,
        )?)
    }
}

pub(super) fn has_window(projections: &[ProjectionPlan]) -> bool {
    projections.iter().any(|p| expr_has_window(&p.expr))
}

pub(super) fn prepare_window_plan(projections: &[ProjectionPlan]) -> PreparedWindowPlan {
    let labels = projection_columns(projections);
    let mut slots = Vec::new();
    let mut rewritten = Vec::with_capacity(projections.len());
    for (idx, projection) in projections.iter().enumerate() {
        let (expr, changed) = rewrite_window_expr(&projection.expr, &mut slots);
        let mut projection = projection.clone();
        projection.expr = expr;
        if changed && projection.alias.is_none() {
            projection.alias = Some(labels[idx].clone());
        }
        rewritten.push(projection);
    }
    PreparedWindowPlan {
        slots,
        projections: rewritten,
    }
}

fn execute_window_plan(
    engine: &Engine,
    plan: &PreparedWindowPlan,
    mut input: SpillBuffer,
    input_schema: &RowSchema,
    work_mem_bytes: usize,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SpillBuffer, SQLError> {
    let mut schema = input_schema.clone();
    for slot in &plan.slots {
        let expression = ScalarExpr::WindowCall {
            name: slot.name.clone(),
            args: slot.args.clone(),
            spec: slot.spec.clone(),
        };
        let slot_type =
            uqa_execution::scalar_type_with_resolver(&expression, &schema, params, engine)?;
        input = execute_spilled_window_slot(
            engine,
            slot,
            input,
            &schema,
            slot_type.clone(),
            work_mem_bytes,
            params,
            ctes,
        )?;
        if schema.internal_slot(slot.column).is_none() {
            schema = RowSchema::append_internal_typed(&schema, &[(slot.column, slot_type)]);
        }
    }

    Ok(input)
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps window frame inputs aligned"
)]
fn execute_spilled_window_slot(
    engine: &Engine,
    slot: &WindowSlot,
    input: SpillBuffer,
    schema: &RowSchema,
    slot_type: Option<uqa_sql::ColumnType>,
    work_mem_bytes: usize,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<SpillBuffer, SQLError> {
    use super::select::EngineExpressionEvaluator;

    let scan: Box<dyn PhysicalOperator + '_> = Box::new(SpillScan::new(schema.clone(), input));
    let mut keys = slot
        .spec
        .partition_by
        .iter()
        .cloned()
        .map(|expr| SortKey {
            expr,
            descending: false,
            nulls_first: None,
        })
        .collect::<Vec<_>>();
    keys.extend(slot.spec.order_by.iter().map(|order| {
        SortKey {
            expr: order.expr.clone(),
            descending: order.descending,
            nulls_first: order
                .nulls
                .map(|nulls| matches!(nulls, uqa_sql::ast::NullsOrder::First)),
        }
    }));
    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    let phase_budget = (work_mem_bytes / 3).max(1);
    let mut sorted = ExternalSort::new(scan, keys, evaluator, None, phase_budget);
    sorted.open().map_err(exec_to_sql_error)?;

    let hook = ScopedEngineHook::new(engine, ctes);
    let subquery_arena = PlanSubqueryArena::new(&ctes.scalar_subqueries, Some(&hook));
    let partition_schema = sorted.row_schema().clone();
    let mut partition = IndexedSpill::new(partition_schema.clone()).map_err(exec_to_sql_error)?;
    let mut partition_key: Option<Vec<Value>> = None;
    let mut output = SpillBuffer::new(phase_budget);
    let output_schema =
        RowSchema::append_internal_typed(&partition_schema, &[(slot.column, slot_type)]);
    if output_schema.columns() != schema.columns()
        || output_schema.internal_slot(slot.column).is_none()
    {
        return Err(SQLError::Internal(format!(
            "window output schema mismatch: expected {:?}, got {:?}",
            schema.columns(),
            output_schema.columns(),
        )));
    }

    let execution = (|| -> Result<(), SQLError> {
        while let Some(batch) = sorted.next().map_err(exec_to_sql_error)? {
            for row in batch.rows {
                let view = batch.schema.view(&row);
                let context = ScalarEvalContext::from_row_lookup(&view, params)
                    .with_function_hook(&hook)
                    .with_subquery_runner(&subquery_arena)
                    .with_physical_outer_row(&batch.schema, &row);
                let key = slot
                    .spec
                    .partition_by
                    .iter()
                    .map(|expression| eval_scalar(expression, &context))
                    .collect::<Result<Vec<_>, _>>()?;
                if partition_key
                    .as_ref()
                    .is_some_and(|current| current != &key)
                {
                    emit_window_partition(
                        slot,
                        &mut partition,
                        &output_schema,
                        &mut output,
                        params,
                        &hook,
                        &subquery_arena,
                    )?;
                    partition =
                        IndexedSpill::new(partition_schema.clone()).map_err(exec_to_sql_error)?;
                }
                partition_key = Some(key);
                partition.push(&row).map_err(exec_to_sql_error)?;
            }
        }
        if !partition.is_empty() {
            emit_window_partition(
                slot,
                &mut partition,
                &output_schema,
                &mut output,
                params,
                &hook,
                &subquery_arena,
            )?;
        }
        Ok(())
    })();
    let close = sorted.close().map_err(exec_to_sql_error);
    combine_execution_and_close(execution, close, "window sort")?;
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

#[expect(
    clippy::too_many_lines,
    reason = "preserves partition peer and frame order"
)]
fn emit_window_partition(
    slot: &WindowSlot,
    partition: &mut IndexedSpill,
    schema: &RowSchema,
    output: &mut SpillBuffer,
    params: &[SQLParam],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
) -> Result<(), SQLError> {
    use uqa_sql::ast::FrameMode;

    let len = partition.len();
    if len == 0 {
        return Ok(());
    }
    let partition_schema = partition.row_schema().clone();
    if schema.columns() != partition_schema.columns() || schema.internal_slot(slot.column).is_none()
    {
        return Err(SQLError::Internal(format!(
            "window output schema mismatch: expected {:?}, got {:?}",
            schema.columns(),
            partition_schema.columns(),
        )));
    }
    let name = slot.name.to_ascii_lowercase();
    let first_row = partition.get(0).map_err(exec_to_sql_error)?;
    let lag_lead = if matches!(name.as_str(), "lag" | "lead") {
        let target = slot.args.first().ok_or_else(|| SQLError::BadArity {
            name: name.clone(),
            expected: ">=1".into(),
            actual: 0,
        })?;
        let offset = match slot.args.get(1) {
            None => 1,
            Some(expression) => {
                match evaluate_on_row(
                    expression,
                    &partition_schema,
                    &first_row,
                    params,
                    eval_hook,
                    subquery_runner,
                )? {
                    Value::Int(offset) => offset,
                    value => {
                        return Err(SQLError::TypeMismatch(format!(
                            "lag/lead offset must be integer, got {value:?}"
                        )))
                    }
                }
            }
        };
        let default = slot.args.get(2).map_or(Ok(Value::Null), |expression| {
            evaluate_on_row(
                expression,
                &partition_schema,
                &first_row,
                params,
                eval_hook,
                subquery_runner,
            )
        })?;
        Some((target.clone(), offset, default))
    } else {
        None
    };
    let ntile_buckets = if name == "ntile" {
        match slot.args.first() {
            Some(expression) => {
                match evaluate_on_row(
                    expression,
                    &partition_schema,
                    &first_row,
                    params,
                    eval_hook,
                    subquery_runner,
                )? {
                    Value::Int(buckets) if buckets > 0 => {
                        Some(u64::try_from(buckets).map_err(|_| {
                            SQLError::TypeMismatch("ntile bucket count exceeds u64".into())
                        })?)
                    }
                    value => {
                        return Err(SQLError::TypeMismatch(format!(
                            "ntile bucket count must be positive integer, got {value:?}"
                        )))
                    }
                }
            }
            None => {
                return Err(SQLError::BadArity {
                    name: "ntile".into(),
                    expected: "1".into(),
                    actual: 0,
                })
            }
        }
    } else {
        None
    };

    let aggregate_name =
        matches!(name.as_str(), "sum" | "count" | "avg" | "min" | "max").then_some(name.as_str());
    let frame = slot.spec.frame.as_ref().map_or_else(
        || {
            if slot.spec.order_by.is_empty() {
                None
            } else {
                Some((
                    FrameMode::Rows,
                    ScalarFrameBound::UnboundedPreceding,
                    ScalarFrameBound::CurrentRow,
                ))
            }
        },
        |frame| Some((frame.mode, frame.start.clone(), frame.end.clone())),
    );
    let mut whole_partition_value = None;
    let mut prefix_accumulator = None;
    if let Some(aggregate_name) = aggregate_name {
        if frame.is_none() {
            let mut accumulator = AggregateAccumulator::builtin(aggregate_name);
            for index in 0..len {
                let row = partition.get(index).map_err(exec_to_sql_error)?;
                let value = window_aggregate_argument(
                    &name,
                    &slot.args,
                    &partition_schema,
                    &row,
                    params,
                    eval_hook,
                    subquery_runner,
                )?;
                accumulator.observe(&value)?;
            }
            whole_partition_value = Some(aggregate_value(aggregate_name, &accumulator)?);
        } else if matches!(
            frame,
            Some((
                FrameMode::Rows,
                ScalarFrameBound::UnboundedPreceding,
                ScalarFrameBound::CurrentRow
            ))
        ) {
            prefix_accumulator = Some(AggregateAccumulator::builtin(aggregate_name));
        }
    }

    let mut previous_order_key: Option<Vec<Value>> = None;
    let mut rank = 0_i64;
    let mut dense_rank = 0_i64;
    let mut pending = Vec::with_capacity(uqa_execution::batch::DEFAULT_BATCH_SIZE);
    for index in 0..len {
        let row = partition.get(index).map_err(exec_to_sql_error)?;
        let order_key = evaluate_order_key(
            &slot.spec.order_by,
            &partition_schema,
            &row,
            params,
            eval_hook,
            subquery_runner,
        )?;
        let value = match name.as_str() {
            "row_number" => Value::Int(window_position(index, "row_number")?),
            "rank" => {
                if previous_order_key.as_ref() != Some(&order_key) {
                    rank = window_position(index, "rank")?;
                }
                Value::Int(rank)
            }
            "dense_rank" => {
                if previous_order_key.as_ref() != Some(&order_key) {
                    dense_rank = dense_rank.checked_add(1).ok_or_else(|| {
                        SQLError::TypeMismatch("dense_rank result overflow".into())
                    })?;
                }
                Value::Int(dense_rank)
            }
            "lag" | "lead" => {
                let (target, offset, default) = lag_lead.as_ref().ok_or_else(|| {
                    SQLError::Internal("lag/lead metadata was not initialized".into())
                })?;
                let direction = if name == "lag" { -1_i128 } else { 1_i128 };
                let target_index = i128::from(index) + direction * i128::from(*offset);
                if target_index < 0 || target_index >= i128::from(len) {
                    default.clone()
                } else {
                    let target_row = partition
                        .get(u64::try_from(target_index).map_err(|_| {
                            SQLError::Internal("lag/lead target index is out of range".into())
                        })?)
                        .map_err(exec_to_sql_error)?;
                    evaluate_on_row(
                        target,
                        &partition_schema,
                        &target_row,
                        params,
                        eval_hook,
                        subquery_runner,
                    )?
                }
            }
            "ntile" => Value::Int(window_ntile(
                index,
                len,
                ntile_buckets.ok_or_else(|| {
                    SQLError::Internal("ntile metadata was not initialized".into())
                })?,
            )?),
            "sum" | "count" | "avg" | "min" | "max" => {
                if let Some(value) = whole_partition_value.as_ref() {
                    value.clone()
                } else if let Some(accumulator) = prefix_accumulator.as_mut() {
                    let value = window_aggregate_argument(
                        &name,
                        &slot.args,
                        &partition_schema,
                        &row,
                        params,
                        eval_hook,
                        subquery_runner,
                    )?;
                    accumulator.observe(&value)?;
                    aggregate_value(&name, accumulator)?
                } else {
                    let (mode, start, end) = frame.as_ref().ok_or_else(|| {
                        SQLError::Internal(
                            "framed aggregate window metadata was not initialized".into(),
                        )
                    })?;
                    evaluate_spilled_window_frame(
                        &name,
                        &slot.args,
                        &slot.spec,
                        partition,
                        index,
                        *mode,
                        start,
                        end,
                        params,
                        eval_hook,
                        subquery_runner,
                    )?
                }
            }
            other => {
                return Err(SQLError::UnknownFunction(format!(
                    "window function `{other}` is not supported"
                )))
            }
        };
        previous_order_key = Some(order_key);
        pending.push(row.append_values(vec![value]));
        if pending.len() == uqa_execution::batch::DEFAULT_BATCH_SIZE {
            output
                .push(Batch::from_physical_rows(
                    schema.clone(),
                    std::mem::take(&mut pending),
                ))
                .map_err(exec_to_sql_error)?;
            pending = Vec::with_capacity(uqa_execution::batch::DEFAULT_BATCH_SIZE);
        }
    }
    if !pending.is_empty() {
        output
            .push(Batch::from_physical_rows(schema.clone(), pending))
            .map_err(exec_to_sql_error)?;
    }
    Ok(())
}

fn evaluate_on_row(
    expression: &ScalarExpr,
    schema: &RowSchema,
    row: &PhysicalRow,
    params: &[SQLParam],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
) -> Result<Value, SQLError> {
    let view = schema.view(row);
    let context = ScalarEvalContext::from_row_lookup(&view, params)
        .with_function_hook(eval_hook)
        .with_subquery_runner(subquery_runner)
        .with_physical_outer_row(schema, row);
    eval_scalar(expression, &context)
}

fn evaluate_order_key(
    order: &[ScalarOrder],
    schema: &RowSchema,
    row: &PhysicalRow,
    params: &[SQLParam],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
) -> Result<Vec<Value>, SQLError> {
    order
        .iter()
        .map(|order| evaluate_on_row(&order.expr, schema, row, params, eval_hook, subquery_runner))
        .collect()
}

fn window_position(index: u64, function: &str) -> Result<i64, SQLError> {
    let position = index
        .checked_add(1)
        .ok_or_else(|| SQLError::TypeMismatch(format!("{function} result overflow")))?;
    i64::try_from(position)
        .map_err(|_| SQLError::TypeMismatch(format!("{function} result exceeds BIGINT")))
}

fn window_ntile(index: u64, rows: u64, buckets: u64) -> Result<i64, SQLError> {
    if buckets == 0 {
        return Err(SQLError::TypeMismatch(
            "ntile bucket count must be positive".into(),
        ));
    }
    let base = rows / buckets;
    let extra = rows % buckets;
    let larger_rows = if extra == 0 {
        0
    } else {
        base.checked_add(1)
            .and_then(|value| value.checked_mul(extra))
            .ok_or_else(|| SQLError::TypeMismatch("ntile partition size overflow".into()))?
    };
    let bucket = if index < larger_rows {
        index
            .checked_div(
                base.checked_add(1)
                    .ok_or_else(|| SQLError::TypeMismatch("ntile bucket width overflow".into()))?,
            )
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| SQLError::TypeMismatch("ntile bucket number overflow".into()))?
    } else if base == 0 {
        extra.max(1)
    } else {
        extra
            .checked_add(
                (index - larger_rows)
                    .checked_div(base)
                    .ok_or_else(|| SQLError::TypeMismatch("ntile bucket width is zero".into()))?,
            )
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| SQLError::TypeMismatch("ntile bucket number overflow".into()))?
    };
    i64::try_from(bucket)
        .map_err(|_| SQLError::TypeMismatch("ntile bucket number exceeds BIGINT".into()))
}

fn window_aggregate_argument(
    name: &str,
    args: &[ScalarExpr],
    schema: &RowSchema,
    row: &PhysicalRow,
    params: &[SQLParam],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
) -> Result<Value, SQLError> {
    if name == "count" && (args.is_empty() || matches!(args, [ScalarExpr::Star])) {
        return Ok(Value::Int(1));
    }
    args.first().map_or(Ok(Value::Int(1)), |expression| {
        evaluate_on_row(expression, schema, row, params, eval_hook, subquery_runner)
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps window frame inputs aligned"
)]
fn evaluate_spilled_window_frame(
    name: &str,
    args: &[ScalarExpr],
    spec: &ScalarWindowSpec,
    partition: &mut IndexedSpill,
    current: u64,
    mode: uqa_sql::ast::FrameMode,
    start_bound: &ScalarFrameBound,
    end_bound: &ScalarFrameBound,
    params: &[SQLParam],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
) -> Result<Value, SQLError> {
    let partition_schema = partition.row_schema().clone();
    let start = resolve_spilled_frame_bound(
        partition,
        current,
        spec,
        mode,
        start_bound,
        true,
        params,
        eval_hook,
        subquery_runner,
    )?;
    let end = resolve_spilled_frame_bound(
        partition,
        current,
        spec,
        mode,
        end_bound,
        false,
        params,
        eval_hook,
        subquery_runner,
    )?;
    let mut accumulator = AggregateAccumulator::builtin(name);
    if start <= end && start < i128::from(partition.len()) && end >= 0 {
        let max_index = partition.len().checked_sub(1).ok_or_else(|| {
            SQLError::Internal("non-empty window frame lost its partition row".into())
        })?;
        let first = u64::try_from(start.max(0))
            .map_err(|_| SQLError::TypeMismatch("window frame start is out of range".into()))?;
        let last = u64::try_from(end.min(i128::from(max_index)))
            .map_err(|_| SQLError::TypeMismatch("window frame end is out of range".into()))?;
        for index in first..=last {
            let row = partition.get(index).map_err(exec_to_sql_error)?;
            let value = window_aggregate_argument(
                name,
                args,
                &partition_schema,
                &row,
                params,
                eval_hook,
                subquery_runner,
            )?;
            accumulator.observe(&value)?;
        }
    }
    aggregate_value(name, &accumulator)
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps window frame inputs aligned"
)]
#[expect(
    clippy::too_many_lines,
    reason = "preserves partition peer and frame order"
)]
fn resolve_spilled_frame_bound(
    partition: &mut IndexedSpill,
    current: u64,
    spec: &ScalarWindowSpec,
    mode: uqa_sql::ast::FrameMode,
    bound: &ScalarFrameBound,
    is_start: bool,
    params: &[SQLParam],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
) -> Result<i128, SQLError> {
    use uqa_sql::ast::FrameMode;

    let partition_schema = partition.row_schema().clone();
    let len = i128::from(partition.len());
    let current_i128 = i128::from(current);
    if !matches!(mode, FrameMode::Range) {
        let row = partition.get(current).map_err(exec_to_sql_error)?;
        return Ok(match bound {
            ScalarFrameBound::UnboundedPreceding => 0,
            ScalarFrameBound::UnboundedFollowing => len - 1,
            ScalarFrameBound::CurrentRow => current_i128,
            ScalarFrameBound::Preceding(expression) => {
                let offset = eval_frame_offset(
                    expression,
                    &partition_schema,
                    &row,
                    params,
                    eval_hook,
                    subquery_runner,
                )?;
                (current_i128 - i128::from(offset)).max(0)
            }
            ScalarFrameBound::Following(expression) => {
                let offset = eval_frame_offset(
                    expression,
                    &partition_schema,
                    &row,
                    params,
                    eval_hook,
                    subquery_runner,
                )?;
                (current_i128 + i128::from(offset)).min(len - 1)
            }
        });
    }

    match bound {
        ScalarFrameBound::UnboundedPreceding => Ok(0),
        ScalarFrameBound::UnboundedFollowing => Ok(len - 1),
        ScalarFrameBound::CurrentRow => {
            let current_row = partition.get(current).map_err(exec_to_sql_error)?;
            let current_key = evaluate_order_key(
                &spec.order_by,
                &partition_schema,
                &current_row,
                params,
                eval_hook,
                subquery_runner,
            )?;
            let mut peer = current;
            if is_start {
                while peer > 0 {
                    let row = partition.get(peer - 1).map_err(exec_to_sql_error)?;
                    if evaluate_order_key(
                        &spec.order_by,
                        &partition_schema,
                        &row,
                        params,
                        eval_hook,
                        subquery_runner,
                    )? != current_key
                    {
                        break;
                    }
                    peer -= 1;
                }
            } else {
                while peer + 1 < partition.len() {
                    let row = partition.get(peer + 1).map_err(exec_to_sql_error)?;
                    if evaluate_order_key(
                        &spec.order_by,
                        &partition_schema,
                        &row,
                        params,
                        eval_hook,
                        subquery_runner,
                    )? != current_key
                    {
                        break;
                    }
                    peer += 1;
                }
            }
            Ok(i128::from(peer))
        }
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            let current_row = partition.get(current).map_err(exec_to_sql_error)?;
            let offset = eval_frame_offset(
                expression,
                &partition_schema,
                &current_row,
                params,
                eval_hook,
                subquery_runner,
            )? as f64;
            let current_key = evaluate_order_key(
                &spec.order_by,
                &partition_schema,
                &current_row,
                params,
                eval_hook,
                subquery_runner,
            )?;
            let current_value = numeric_value(current_key.first()).ok_or_else(|| {
                SQLError::TypeMismatch(
                    "RANGE offset frame requires a numeric first ORDER BY key".into(),
                )
            })?;
            let target = if matches!(bound, ScalarFrameBound::Preceding(_)) {
                current_value - offset
            } else {
                current_value + offset
            };
            let mut resolved = if is_start { len } else { -1 };
            for index in 0..partition.len() {
                let row = partition.get(index).map_err(exec_to_sql_error)?;
                let key = evaluate_order_key(
                    &spec.order_by,
                    &partition_schema,
                    &row,
                    params,
                    eval_hook,
                    subquery_runner,
                )?;
                let Some(value) = numeric_value(key.first()) else {
                    continue;
                };
                if is_start {
                    if value >= target {
                        resolved = i128::from(index);
                        break;
                    }
                } else if value <= target {
                    resolved = i128::from(index);
                } else {
                    break;
                }
            }
            Ok(resolved)
        }
    }
}

fn numeric_value(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Int(value)) => Some(*value as f64),
        Some(Value::Float(value)) => Some(*value),
        Some(Value::Decimal(value)) => value.to_f64(),
        _ => None,
    }
}

fn eval_frame_offset(
    expr: &ScalarExpr,
    schema: &RowSchema,
    row: &PhysicalRow,
    params: &[SQLParam],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
) -> Result<i64, SQLError> {
    let view = schema.view(row);
    let ctx = ScalarEvalContext::from_row_lookup(&view, params)
        .with_function_hook(eval_hook)
        .with_subquery_runner(subquery_runner)
        .with_physical_outer_row(schema, row);
    match eval_scalar(expr, &ctx)? {
        Value::Int(offset) if offset >= 0 => Ok(offset),
        Value::Float(offset) => float_frame_offset(offset),
        other => Err(SQLError::TypeMismatch(format!(
            "frame offset must be a non-negative integer, got {other:?}"
        ))),
    }
}

fn float_frame_offset(offset: f64) -> Result<i64, SQLError> {
    const I64_UPPER_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    if !offset.is_finite() || offset < 0.0 || offset.fract() != 0.0 || offset >= I64_UPPER_EXCLUSIVE
    {
        return Err(SQLError::TypeMismatch(format!(
            "frame offset must be a finite non-negative integer within BIGINT range, got {offset}"
        )));
    }
    Ok(offset as i64)
}

#[cfg(test)]
mod tests;
