//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL window function evaluation. Window calls that partition and order their rows the same way share one sort, as the functions of one `WindowAgg` do, and each sorted partition is evaluated row by row with the frame semantics of `nodeWindowAgg.c`.

use crate::scalar::plan::{PlanSubqueryArena, QueryExpressionContext};
use crate::spill::BufferedIndexedSpill;
use crate::RowSchemaExecution;
use crate::{
    eval_scalar, Batch, ExternalSort, PhysicalOperator, PhysicalRow, RowSchema, ScalarEvalContext,
    ScalarExpr, ScalarFrameBound, ScalarOrder, ScalarSubqueryRunner, ScalarWindowSpec, SortKey,
    SpillBuffer, SpillScan, WindowExecutor,
};
use uqa_core::Value;
use uqa_sql::ast::{ColumnType, FrameExclusion, FrameMode, NullsOrder};
use uqa_sql::plan::ProjectionPlan;
use uqa_sql::semantics::projection_columns;
use uqa_sql::{SQLError, SQLParam};

mod aggregates;
mod frame;
mod functions;
mod partition;
mod planning;

pub use planning::expr_has_window;
use planning::rewrite_window_expr;

use frame::{BoundKind, CurrentRow, FrameSpec};
use functions::{WindowFunction, WindowFunctionState};
use partition::PartitionRows;

#[derive(Clone)]
struct WindowSlot {
    column: uqa_sql::ast::InternalColumnRef,
    name: String,
    args: Vec<ScalarExpr>,
    spec: ScalarWindowSpec,
    /// An aggregate's `FILTER (WHERE ...)` condition.
    filter: Option<ScalarExpr>,
}

impl WindowSlot {
    fn call(&self) -> ScalarExpr {
        ScalarExpr::WindowCall {
            name: self.name.clone(),
            args: self.args.clone(),
            spec: self.spec.clone(),
            filter: self.filter.clone().map(Box::new),
            modifiers: uqa_sql::ast::WindowCallModifiers::default(),
        }
    }
}

/// Window calls that partition and order their rows alike, evaluated over one sort.
struct WindowPass {
    partition_by: Vec<ScalarExpr>,
    order_by: Vec<ScalarOrder>,
    slots: Vec<WindowSlot>,
}

pub struct PreparedWindowPlan {
    passes: Vec<WindowPass>,
    projections: Vec<ProjectionPlan>,
}

impl PreparedWindowPlan {
    pub fn projections(&self) -> &[ProjectionPlan] {
        &self.projections
    }

    /// The input schema with one internal column per window call, in pass order.
    pub fn output_schema(
        &self,
        context: &dyn uqa_sql::FunctionTypeResolver,
        input: &RowSchema,
        params: &[SQLParam],
    ) -> Result<RowSchema, SQLError> {
        let mut schema = input.clone();
        for pass in &self.passes {
            let types = slot_types(pass, context, &schema, params)?;
            schema = pass_output_schema(&schema, &types);
        }
        Ok(schema)
    }
}

pub struct PhysicalWindowExecutor<'a> {
    context: std::sync::Arc<dyn QueryExpressionContext + 'a>,
    plan: PreparedWindowPlan,
    params: &'a [SQLParam],

    schema: RowSchema,
    work_mem_bytes: usize,
    input: Option<SpillBuffer>,
}

impl<'a> PhysicalWindowExecutor<'a> {
    pub fn new(
        context: std::sync::Arc<dyn QueryExpressionContext + 'a>,
        plan: PreparedWindowPlan,
        params: &'a [SQLParam],

        schema: RowSchema,
        work_mem_bytes: usize,
    ) -> Self {
        Self {
            context,
            plan,
            params,

            schema,
            work_mem_bytes,
            input: Some(SpillBuffer::new((work_mem_bytes / 3).max(1))),
        }
    }
}

impl WindowExecutor for PhysicalWindowExecutor<'_> {
    fn consume(&mut self, batch: Batch) -> crate::ExecResult<()> {
        self.input
            .as_mut()
            .ok_or_else(|| crate::ExecError::Other("window executor already finalized".into()))?
            .push(batch)?;
        Ok(())
    }

    fn finish(&mut self) -> crate::ExecResult<SpillBuffer> {
        let mut input = self
            .input
            .take()
            .ok_or_else(|| crate::ExecError::Other("window executor already finalized".into()))?;
        let mut schema = self.schema.clone();
        for pass in &self.plan.passes {
            let types = slot_types(pass, self.context.as_ref(), &schema, self.params)?;
            input = execute_window_pass(
                self.context.as_ref(),
                pass,
                input,
                &schema,
                &types,
                self.work_mem_bytes,
                self.params,
            )?;
            schema = pass_output_schema(&schema, &types);
        }
        Ok(input)
    }
}

/// Each pass sorts into a compact layout before appending its window values.
/// Keep the declared layout and the next pass's input aligned with those rows.
fn pass_output_schema(
    input: &RowSchema,
    types: &[(uqa_sql::ast::InternalColumnRef, Option<ColumnType>)],
) -> RowSchema {
    let (partition, _) = input.canonical_projection();
    RowSchema::append_internal_typed(&partition, types)
}

pub fn has_window(projections: &[ProjectionPlan]) -> bool {
    projections.iter().any(|p| expr_has_window(&p.expr))
}

pub fn prepare_window_plan(projections: &[ProjectionPlan]) -> PreparedWindowPlan {
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
    let mut passes: Vec<WindowPass> = Vec::new();
    for slot in slots {
        match passes.iter_mut().find(|pass| {
            pass.partition_by == slot.spec.partition_by && pass.order_by == slot.spec.order_by
        }) {
            Some(pass) => pass.slots.push(slot),
            None => passes.push(WindowPass {
                partition_by: slot.spec.partition_by.clone(),
                order_by: slot.spec.order_by.clone(),
                slots: vec![slot],
            }),
        }
    }
    PreparedWindowPlan {
        passes,
        projections: rewritten,
    }
}

fn slot_types(
    pass: &WindowPass,
    context: &dyn uqa_sql::FunctionTypeResolver,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Vec<(uqa_sql::ast::InternalColumnRef, Option<ColumnType>)>, SQLError> {
    pass.slots
        .iter()
        .map(|slot| {
            crate::scalar_type_with_resolver(&slot.call(), schema, params, context)
                .map(|ty| (slot.column, ty))
        })
        .collect()
}

/// One pass's window calls prepared for evaluation, with the frame offsets each still has to evaluate.
struct PreparedSlot {
    state: WindowFunctionState,
    start_offset: Option<FrameOffset>,
    end_offset: Option<FrameOffset>,
}

/// A frame offset expression and the type `transformFrameOffset` coerces it to.
struct FrameOffset {
    expression: ScalarExpr,
    target: ColumnType,
}

fn execute_window_pass(
    context: &dyn QueryExpressionContext,
    pass: &WindowPass,
    input: SpillBuffer,
    schema: &RowSchema,
    types: &[(uqa_sql::ast::InternalColumnRef, Option<ColumnType>)],
    work_mem_bytes: usize,
    params: &[SQLParam],
) -> Result<SpillBuffer, SQLError> {
    let phase_budget = (work_mem_bytes / 3).max(1);
    let mut slots = pass
        .slots
        .iter()
        .map(|slot| prepare_slot(context, slot, schema, params, phase_budget))
        .collect::<Result<Vec<_>, _>>()?;
    let scan: Box<dyn PhysicalOperator + '_> = Box::new(SpillScan::new(schema.clone(), input));
    let mut keys = pass
        .partition_by
        .iter()
        .cloned()
        .map(|expr| SortKey {
            expr,
            descending: false,
            nulls_first: None,
        })
        .collect::<Vec<_>>();
    keys.extend(pass.order_by.iter().map(|order| SortKey {
        expr: order.expr.clone(),
        descending: order.descending,
        nulls_first: order.nulls.map(|nulls| matches!(nulls, NullsOrder::First)),
    }));
    let evaluator = context.expression_evaluator(params);
    let mut sorted = ExternalSort::new(scan, keys, evaluator, None, phase_budget);
    sorted.open().map_err(exec_to_sql_error)?;

    let hook = context;
    let subquery_arena = PlanSubqueryArena::new(context.subquery_plans(), Some(hook));
    let partition_schema = sorted.row_schema().clone();
    let output_schema = RowSchema::append_internal_typed(&partition_schema, types);
    let mut output = SpillBuffer::new(phase_budget);
    let mut offsets_evaluated = false;

    let execution = (|| -> Result<(), SQLError> {
        let mut partition = BufferedIndexedSpill::new(partition_schema.clone(), phase_budget);
        let mut partition_key: Option<Vec<Value>> = None;
        while let Some(batch) = sorted.next().map_err(exec_to_sql_error)? {
            for row in batch.rows {
                let key = evaluate_all(
                    &pass.partition_by,
                    &batch.schema,
                    &row,
                    params,
                    hook,
                    &subquery_arena,
                )?;
                let same_partition = match &partition_key {
                    Some(current) => crate::relational::equal_sort_key_values(
                        current,
                        &key,
                        uqa_sql::expr::EngineHook::enum_labels(hook),
                    )
                    .map_err(exec_to_sql_error)?,
                    None => true,
                };
                if !same_partition {
                    emit_partition(
                        pass,
                        &mut slots,
                        &mut partition,
                        (&output_schema, &mut output),
                        (params, hook, &subquery_arena),
                        &mut offsets_evaluated,
                    )?;
                    partition = BufferedIndexedSpill::new(partition_schema.clone(), phase_budget);
                }
                partition_key = Some(key);
                partition.push(&row).map_err(exec_to_sql_error)?;
            }
        }
        if partition.is_empty() {
            // `calculate_frame_offsets` runs before the first input row is read, so an invalid constant offset is reported even when there are no rows.
            if !offsets_evaluated {
                let empty = RowSchema::default();
                let row = PhysicalRow::default();
                let view = empty.view(&row);
                let evaluation = ScalarEvalContext::from_row_lookup(&view, params)
                    .with_function_hook(hook)
                    .with_subquery_runner(&subquery_arena);
                for slot in &mut slots {
                    if slot.offsets_reference_rows() {
                        continue;
                    }
                    slot.evaluate_offsets(&evaluation)?;
                }
            }
        } else {
            emit_partition(
                pass,
                &mut slots,
                &mut partition,
                (&output_schema, &mut output),
                (params, hook, &subquery_arena),
                &mut offsets_evaluated,
            )?;
        }
        Ok(())
    })();
    let close = sorted.close().map_err(exec_to_sql_error);
    combine_execution_and_close(execution, close, "window sort")?;
    Ok(output)
}

fn prepare_slot(
    context: &dyn QueryExpressionContext,
    slot: &WindowSlot,
    schema: &RowSchema,
    params: &[SQLParam],
    budget_bytes: usize,
) -> Result<PreparedSlot, SQLError> {
    let spec = &slot.spec;
    let (mode, start, end, exclusion) = spec.frame.as_ref().map_or(
        // Without a frame clause the frame is RANGE UNBOUNDED PRECEDING, which ends with the current row's last peer.
        (
            FrameMode::Range,
            BoundKind::UnboundedPreceding,
            BoundKind::CurrentRow,
            FrameExclusion::NoOthers,
        ),
        |frame| {
            (
                frame.mode,
                bound_kind(&frame.start),
                bound_kind(&frame.end),
                frame.exclusion,
            )
        },
    );
    let (ascending, nulls_first) = spec.order_by.first().map_or((true, false), |order| {
        (
            !order.descending,
            order
                .nulls
                .map_or(order.descending, |nulls| nulls == NullsOrder::First),
        )
    });
    let offset = |bound: Option<&ScalarFrameBound>| -> Result<Option<FrameOffset>, SQLError> {
        let Some(ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression)) =
            bound
        else {
            return Ok(None);
        };
        let target = if mode == FrameMode::Range {
            let order = spec.order_by.first().ok_or_else(|| {
                SQLError::Internal("RANGE frame offset has no ordering column".into())
            })?;
            let order_type =
                crate::scalar_type_with_resolver(&order.expr, schema, params, context)?;
            let offset_type =
                crate::scalar_type_with_resolver(expression, schema, params, context)?;
            let offset_type = uqa_sql::effective_overload_argument_type_with_params(
                expression,
                offset_type,
                params,
            );
            uqa_sql::range_frame_offset_type(order_type.as_ref(), offset_type.as_ref())?
        } else {
            ColumnType::BigInteger
        };
        Ok(Some(FrameOffset {
            expression: (**expression).clone(),
            target,
        }))
    };
    let start_offset = offset(spec.frame.as_ref().map(|frame| &frame.start))?;
    let end_offset = offset(spec.frame.as_ref().map(|frame| &frame.end))?;
    let frame = FrameSpec {
        mode,
        start,
        end,
        exclusion,
        start_offset: None,
        end_offset: None,
        ascending,
        nulls_first,
    };
    let function = window_function(context, slot, schema, params, budget_bytes)?;
    Ok(PreparedSlot {
        state: WindowFunctionState::new(function, frame),
        start_offset,
        end_offset,
    })
}

const fn bound_kind(bound: &ScalarFrameBound) -> BoundKind {
    match bound {
        ScalarFrameBound::UnboundedPreceding => BoundKind::UnboundedPreceding,
        ScalarFrameBound::UnboundedFollowing => BoundKind::UnboundedFollowing,
        ScalarFrameBound::CurrentRow => BoundKind::CurrentRow,
        ScalarFrameBound::Preceding(_) => BoundKind::Preceding,
        ScalarFrameBound::Following(_) => BoundKind::Following,
    }
}

fn window_function(
    context: &dyn QueryExpressionContext,
    slot: &WindowSlot,
    schema: &RowSchema,
    params: &[SQLParam],
    budget_bytes: usize,
) -> Result<WindowFunction, SQLError> {
    let name = uqa_sql::semantics::builtin_function_dispatch_name(&slot.name);
    let bind = |expression: &ScalarExpr| {
        crate::bind_type_introspection_with_resolver(expression.clone(), schema, params, context)
    };
    let argument = |position: usize| {
        slot.args
            .get(position)
            .map(|expression| crate::scalar::PreparedExpressions::scalar(bind(expression)))
    };
    let required = |position: usize| {
        argument(position).ok_or_else(|| SQLError::BadArity {
            name: name.clone(),
            expected: format!(">={}", position + 1),
            actual: slot.args.len(),
        })
    };
    Ok(match name.as_str() {
        "row_number" => WindowFunction::RowNumber,
        "rank" => WindowFunction::Rank,
        "dense_rank" => WindowFunction::DenseRank,
        "percent_rank" => WindowFunction::PercentRank,
        "cume_dist" => WindowFunction::CumeDist,
        "ntile" => WindowFunction::Ntile(required(0)?),
        "lag" | "lead" => WindowFunction::Shift(Box::new(functions::Shift {
            forward: name == "lead",
            target: required(0)?,
            offset: argument(1),
            default: argument(2),
        })),
        "first_value" => WindowFunction::FirstValue(required(0)?),
        "last_value" => WindowFunction::LastValue(required(0)?),
        "nth_value" => WindowFunction::NthValue {
            target: required(0)?,
            position: required(1)?,
        },
        _ => {
            let call = ScalarExpr::Func {
                order_syntax: uqa_sql::ast::FunctionOrderSyntax::Ordinary,
                name: slot.name.clone(),
                binding: None,
                args: slot.args.iter().map(bind).collect(),
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            };
            let template = crate::aggregation::aggregate_accumulator_templates(
                context,
                std::slice::from_ref(&call),
                schema,
                params,
            )?
            .pop()
            .ok_or_else(|| SQLError::Internal("window aggregate lost its accumulator".into()))?;
            let ScalarExpr::Func { args, .. } = call else {
                unreachable!()
            };
            let filter = slot.filter.as_ref().map(bind);
            WindowFunction::Aggregate(Box::new(aggregates::WindowAggregate::new(
                (&slot.name, &args, filter.as_ref()),
                template,
                budget_bytes,
            )))
        }
    })
}

impl PreparedSlot {
    fn offsets_reference_rows(&self) -> bool {
        [&self.start_offset, &self.end_offset]
            .into_iter()
            .flatten()
            .any(|offset| {
                let mut found = false;
                offset.expression.visit(&mut |part| {
                    found |= matches!(
                        part,
                        ScalarExpr::Column(_)
                            | ScalarExpr::QualifiedColumn { .. }
                            | ScalarExpr::Position(_)
                            | ScalarExpr::InternalColumn(_)
                    );
                });
                found
            })
    }

    /// `calculate_frame_offsets`: each offset is evaluated once, must not be NULL, and a `ROWS` or `GROUPS` count must not be negative.
    fn evaluate_offsets(&mut self, context: &ScalarEvalContext<'_>) -> Result<(), SQLError> {
        let mode = self.state.frame_mut().mode;
        let start = self
            .start_offset
            .as_ref()
            .map(|offset| frame_offset_value(offset, mode, "starting", context))
            .transpose()?;
        let end = self
            .end_offset
            .as_ref()
            .map(|offset| frame_offset_value(offset, mode, "ending", context))
            .transpose()?;
        let frame = self.state.frame_mut();
        frame.start_offset = start;
        frame.end_offset = end;
        Ok(())
    }
}

fn frame_offset_value(
    offset: &FrameOffset,
    mode: FrameMode,
    which: &str,
    context: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    let value = eval_scalar(&offset.expression, context)?;
    if matches!(value, Value::Null) {
        return Err(SQLError::Routine {
            sqlstate: "22004".into(),
            message: format!("frame {which} offset must not be null"),
        });
    }
    let value = uqa_sql::expr::cast_value(&value, &offset.target.sql_name())?;
    if mode != FrameMode::Range && matches!(value, Value::Int(count) if count < 0) {
        return Err(SQLError::Routine {
            sqlstate: "22013".into(),
            message: format!("frame {which} offset must not be negative"),
        });
    }
    Ok(value)
}

/// Evaluate every window call of a pass for each row of one sorted partition and append the results to the row.
fn emit_partition(
    pass: &WindowPass,
    slots: &mut [PreparedSlot],
    partition: &mut BufferedIndexedSpill,
    (schema, output): (&RowSchema, &mut SpillBuffer),
    (params, hook, subqueries): (
        &[SQLParam],
        &dyn uqa_sql::expr::EngineHook,
        &dyn ScalarSubqueryRunner,
    ),
    offsets_evaluated: &mut bool,
) -> Result<(), SQLError> {
    let mut rows = PartitionRows::new(partition, &pass.order_by, params, hook, subqueries)?;
    if rows.len() == 0 {
        return Ok(());
    }
    if !*offsets_evaluated {
        // Offsets may use an enclosing query's columns, which every row of the input carries.
        for slot in slots.iter_mut() {
            rows.with_context(0, |context| slot.evaluate_offsets(context))?;
        }
        *offsets_evaluated = true;
    }
    for slot in slots.iter_mut() {
        slot.state.begin_partition();
    }
    let mut current = CurrentRow::new();
    let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
    for position in 0..rows.len() {
        if position > 0 {
            for slot in slots.iter_mut() {
                slot.state.advance();
            }
            current.advance(&mut rows)?;
        }
        let values = slots
            .iter_mut()
            .map(|slot| slot.state.value(&mut current, &mut rows))
            .collect::<Result<Vec<_>, _>>()?;
        pending.push(rows.row(position)?.append_values(values));
        if pending.len() == crate::batch::DEFAULT_BATCH_SIZE {
            output
                .push(Batch::from_physical_rows(
                    schema.clone(),
                    std::mem::take(&mut pending),
                ))
                .map_err(exec_to_sql_error)?;
            pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);
        }
    }
    if !pending.is_empty() {
        output
            .push(Batch::from_physical_rows(schema.clone(), pending))
            .map_err(exec_to_sql_error)?;
    }
    Ok(())
}

fn evaluate_all(
    expressions: &[ScalarExpr],
    schema: &RowSchema,
    row: &PhysicalRow,
    params: &[SQLParam],
    hook: &dyn uqa_sql::expr::EngineHook,
    subqueries: &dyn ScalarSubqueryRunner,
) -> Result<Vec<Value>, SQLError> {
    expressions
        .iter()
        .map(|expression| evaluate_on_row(expression, schema, row, params, hook, subqueries))
        .collect()
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

fn exec_to_sql_error(error: crate::ExecError) -> SQLError {
    match error {
        crate::ExecError::SQL(error) => error,
        crate::ExecError::Other(message) => SQLError::Internal(message),
    }
}

#[cfg(test)]
mod tests;
