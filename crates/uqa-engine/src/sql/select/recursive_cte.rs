//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recursive CTE materialization and spill deduplication.

use super::{
    analyze_recursive_control_step, collect_query_operator, eval_scalar, execute_query_plan_output,
    materialize_plan_ctes, physical_exec_error, physical_work_mem_bytes,
    push_output_filter_into_query_plan, query_plan_output_columns, CtePlan, CteScope, Engine,
    ProjectionPlan, QueryOutput, QueryOutputMode, QueryRows, RelationalPlan, SQLError, SQLParam,
    ScalarExpr, SetOpKind, SourcePlan, Value,
};
use uqa_core::ArrayValue;

/// Iterate the recursive `CtePlan`: take the anchor (LHS of UNION ALL) as
/// the initial row set, then repeatedly evaluate the recursive step
/// (RHS) with the `CtePlan` bound to the *new rows from the previous
/// iteration* (working set), unioning the result back into the total.
/// Caps at 1024 iterations to keep buggy queries from running away.
#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub(in crate::sql) fn materialize_recursive_cte(
    engine: &Engine,
    cte: &CtePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_filter: Option<&(String, ScalarExpr)>,
) -> Result<uqa_execution::SharedSpill, SQLError> {
    if !cte.query.ctes.is_empty() {
        materialize_plan_ctes(engine, &cte.query.ctes, params, ctes)?;
    }

    let RelationalPlan::SetOp {
        kind,
        all,
        left,
        right,
        order_by,
        limit,
        offset,
        ..
    } = &cte.query.root
    else {
        return Err(SQLError::Unsupported(
            "recursive CTE requires a UNION query".into(),
        ));
    };
    if *kind != SetOpKind::Union {
        return Err(SQLError::Unsupported(
            "recursive CTE only supports UNION".into(),
        ));
    }
    reject_recursive_set_ordering(order_by, limit.as_deref(), offset.as_deref())?;

    let declared_columns = (!cte.columns.is_empty()).then_some(cte.columns.as_slice());
    let controls_recursion = cte.search.is_some() || cte.cycle.is_some();
    let (anchor_plan, step_plan) = if controls_recursion {
        ((**left).clone(), (**right).clone())
    } else if let Some((qualifier, filter)) = output_filter {
        let output_columns = declared_columns
            .map(<[String]>::to_vec)
            .or_else(|| query_plan_output_columns(left));
        match output_columns {
            Some(output_columns) => {
                let specialized_anchor = push_output_filter_into_query_plan(
                    engine,
                    left,
                    qualifier,
                    filter,
                    Some(&output_columns),
                )?;
                let specialized_step = push_output_filter_into_query_plan(
                    engine,
                    right,
                    qualifier,
                    filter,
                    Some(&output_columns),
                )?;
                match (specialized_anchor, specialized_step) {
                    (Some(anchor), Some(step)) => (anchor, step),
                    _ => ((**left).clone(), (**right).clone()),
                }
            }
            None => ((**left).clone(), (**right).clone()),
        }
    } else {
        ((**left).clone(), (**right).clone())
    };

    if controls_recursion {
        return materialize_controlled_recursive_cte(
            engine,
            cte,
            params,
            ctes,
            &anchor_plan,
            &step_plan,
            *all,
        );
    }

    let anchor = execute_query_plan_output(
        engine,
        &anchor_plan,
        params,
        ctes,
        QueryOutputMode::SharedSpill,
    )?;
    let anchor_columns = if cte.columns.is_empty() {
        anchor.columns.clone()
    } else {
        cte.columns.clone()
    };
    let mut working = alias_query_output_to_shared(engine, anchor, &anchor_columns)?;
    let anchor_schema = working.row_schema().clone();

    let work_mem = physical_work_mem_bytes(engine.query_runtime_view())?.max(1);
    // The accumulated rows and UNION duplicate state are live together. Give
    // each at most half of work_mem; SharedSpill working sets are disk-only.
    let state_budget = (work_mem / 2).max(1);
    let mut accumulated = uqa_execution::SpillBuffer::new(state_budget);
    let mut seen = (!*all).then(|| uqa_execution::ExactRowSet::new(state_budget));
    if let Some(seen) = seen.as_mut() {
        working = filter_new_recursive_rows(&working, seen)?;
    }

    const MAX_ITERATIONS: usize = 1024;
    let mut iterations = 0usize;
    while working.rows() != 0 {
        if iterations == MAX_ITERATIONS {
            return Err(SQLError::Unsupported(format!(
                "recursive CTE `{}` exceeded {MAX_ITERATIONS} iterations",
                cte.name
            )));
        }
        iterations += 1;

        append_shared_spill(&mut accumulated, &working)?;
        ctes.insert_shared(cte.name.clone(), working);
        let step_result = execute_query_plan_output(
            engine,
            &step_plan,
            params,
            ctes,
            QueryOutputMode::SharedSpill,
        );
        ctes.remove_materialized(&cte.name);
        let step = step_result?;
        working = alias_query_output_to_shared(engine, step, &anchor_columns)?;
        if let Some(seen) = seen.as_mut() {
            working = filter_new_recursive_rows(&working, seen)?;
        }
    }

    accumulated
        .into_shared(anchor_schema)
        .map_err(physical_exec_error)
}

fn reject_recursive_set_ordering(
    order_by: &[uqa_planner::OrderPlan],
    limit: Option<&ScalarExpr>,
    offset: Option<&ScalarExpr>,
) -> Result<(), SQLError> {
    if !order_by.is_empty() {
        return Err(SQLError::Unsupported(
            "ORDER BY in a recursive query is not implemented".into(),
        ));
    }
    if offset.is_some() {
        return Err(SQLError::Unsupported(
            "OFFSET in a recursive query is not implemented".into(),
        ));
    }
    if limit.is_some() {
        return Err(SQLError::Unsupported(
            "LIMIT in a recursive query is not implemented".into(),
        ));
    }
    Ok(())
}

fn materialize_controlled_recursive_cte(
    engine: &Engine,
    cte: &CtePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    anchor_plan: &uqa_planner::QueryPlan,
    step_plan: &uqa_planner::QueryPlan,
    all: bool,
) -> Result<uqa_execution::SharedSpill, SQLError> {
    let anchor = execute_query_plan_output(
        engine,
        anchor_plan,
        params,
        ctes,
        QueryOutputMode::SharedSpill,
    )?;
    let base_columns = if cte.columns.is_empty() {
        anchor.columns.clone()
    } else {
        cte.columns.clone()
    };
    let anchor = alias_query_output_to_shared(engine, anchor, &base_columns)?;
    let base_schema = anchor.row_schema().clone();
    analyze_recursive_control_step(engine, cte, step_plan, base_schema.clone(), params, ctes)?;
    let generated_schema =
        super::extend_cte_generated_schema(engine, cte, base_schema.clone(), params)?;
    let search_positions = cte
        .search
        .as_ref()
        .map(|search| resolve_control_positions(&base_schema, &search.columns, "search"))
        .transpose()?;
    let cycle_positions = cte
        .cycle
        .as_ref()
        .map(|cycle| resolve_control_positions(&base_schema, &cycle.columns, "cycle"))
        .transpose()?;
    let cycle_marks = cte
        .cycle
        .as_ref()
        .map(|cycle| cycle_mark_values(engine, cycle, params, &generated_schema))
        .transpose()?;
    let controlled_step = controlled_recursive_step_plan(cte, step_plan)?;

    let work_mem = physical_work_mem_bytes(engine.query_runtime_view())?.max(1);
    let state_budget = (work_mem / 2).max(1);
    let mut seen = (!all).then(|| uqa_execution::ExactRowSet::new(state_budget));
    let (mut emitted, mut expandable) = annotate_recursive_rows(
        &anchor,
        &generated_schema,
        cte,
        search_positions.as_deref(),
        cycle_positions.as_deref(),
        cycle_marks.as_ref(),
        seen.as_mut(),
        None,
        0,
    )?;

    let mut accumulated = uqa_execution::SpillBuffer::new(state_budget);
    const MAX_ITERATIONS: usize = 1024;
    let mut iterations = 0usize;
    while emitted.rows() != 0 {
        append_shared_spill(&mut accumulated, &emitted)?;
        if expandable.rows() == 0 {
            break;
        }
        if iterations == MAX_ITERATIONS {
            return Err(SQLError::Unsupported(format!(
                "recursive CTE `{}` exceeded {MAX_ITERATIONS} iterations",
                cte.name
            )));
        }
        iterations += 1;
        ctes.insert_shared(cte.name.clone(), expandable);
        let previous_width = ctes.set_recursive_control_width(cte.name.clone(), base_schema.len());
        let step_result = execute_query_plan_output(
            engine,
            &controlled_step.plan,
            params,
            ctes,
            QueryOutputMode::SharedSpill,
        );
        ctes.restore_recursive_control_width(&cte.name, previous_width);
        ctes.remove_materialized(&cte.name);
        let step = step_result?;
        let step = alias_query_output_to_shared(engine, step, &base_columns)?;
        (emitted, expandable) = annotate_recursive_step_rows(
            &step,
            &base_schema,
            &generated_schema,
            cte,
            search_positions.as_deref(),
            cycle_positions.as_deref(),
            cycle_marks.as_ref(),
            seen.as_mut(),
            &controlled_step,
            iterations,
        )?;
    }
    accumulated
        .into_shared(generated_schema)
        .map_err(physical_exec_error)
}

struct ControlledRecursiveStep {
    plan: uqa_planner::QueryPlan,
    parent_search: bool,
    parent_cycle_path: bool,
}

fn controlled_recursive_step_plan(
    cte: &CtePlan,
    step: &uqa_planner::QueryPlan,
) -> Result<ControlledRecursiveStep, SQLError> {
    let mut plan = step.clone();
    let parent_search = cte
        .search
        .as_ref()
        .is_some_and(|search| !search.breadth_first);
    let parent_cycle_path = cte.cycle.is_some();
    if !parent_search && !parent_cycle_path {
        return Ok(ControlledRecursiveStep {
            plan,
            parent_search,
            parent_cycle_path,
        });
    }
    let RelationalPlan::QueryBlock(block) = &mut plan.root else {
        return Err(SQLError::Unsupported(
            "recursive SEARCH/CYCLE term must be one SELECT query block".into(),
        ));
    };
    let qualifier = block
        .from
        .as_ref()
        .and_then(|source| recursive_reference_qualifier(source, &cte.name))
        .ok_or_else(|| {
            SQLError::Unsupported(
                "recursive SEARCH/CYCLE reference must be visible in the recursive term".into(),
            )
        })?;
    if parent_search {
        let search = cte.search.as_ref().expect("depth-first SEARCH");
        block.projections.push(ProjectionPlan {
            expr: ScalarExpr::qualified_column(&qualifier, &search.sequence_column),
            alias: None,
        });
    }
    if parent_cycle_path {
        let cycle = cte.cycle.as_ref().expect("CYCLE clause");
        block.projections.push(ProjectionPlan {
            expr: ScalarExpr::qualified_column(qualifier, &cycle.path_column),
            alias: None,
        });
    }
    Ok(ControlledRecursiveStep {
        plan,
        parent_search,
        parent_cycle_path,
    })
}

fn recursive_reference_qualifier(source: &SourcePlan, cte_name: &str) -> Option<String> {
    match source {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            ..
        } if super::cte_reference_name(name).as_deref() == Some(cte_name) => {
            Some(alias.as_deref().unwrap_or(qualifier).to_string())
        }
        SourcePlan::Join { left, right, .. } => recursive_reference_qualifier(left, cte_name)
            .or_else(|| recursive_reference_qualifier(right, cte_name)),
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => None,
    }
}

fn resolve_control_positions(
    schema: &uqa_execution::RowSchema,
    columns: &[String],
    kind: &str,
) -> Result<Vec<usize>, SQLError> {
    columns
        .iter()
        .map(|column| {
            schema
                .columns()
                .iter()
                .position(|candidate| candidate == column)
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: format!("{kind} column \"{column}\" not in WITH query column list"),
                })
        })
        .collect()
}

fn cycle_mark_values(
    engine: &Engine,
    cycle: &uqa_planner::CteCyclePlan,
    params: &[SQLParam],
    schema: &uqa_execution::RowSchema,
) -> Result<(Value, Value), SQLError> {
    let empty = uqa_execution::RowSchema::default();
    let mark_type = schema
        .columns()
        .iter()
        .position(|column| column == &cycle.mark_column)
        .and_then(|position| schema.column_type(position));
    let value_type =
        uqa_execution::scalar_type_with_resolver(&cycle.mark_value, &empty, params, engine)?;
    let default_type =
        uqa_execution::scalar_type_with_resolver(&cycle.mark_default, &empty, params, engine)?;
    let context = uqa_execution::ScalarEvalContext::new(None, params).with_function_hook(engine);
    let mark = super::coerce_common_context_value(
        eval_scalar(&cycle.mark_value, &context)?,
        value_type.as_ref(),
        mark_type,
    )?;
    let default = super::coerce_common_context_value(
        eval_scalar(&cycle.mark_default, &context)?,
        default_type.as_ref(),
        mark_type,
    )?;
    Ok((mark, default))
}

#[derive(Default)]
struct RecursiveParentState {
    search_sequence: Option<Value>,
    cycle_path: Option<Value>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps SELECT scope inputs aligned"
)]
fn annotate_recursive_rows(
    base_rows: &uqa_execution::SharedSpill,
    generated_schema: &uqa_execution::RowSchema,
    cte: &CtePlan,
    search_positions: Option<&[usize]>,
    cycle_positions: Option<&[usize]>,
    cycle_marks: Option<&(Value, Value)>,
    mut seen: Option<&mut uqa_execution::ExactRowSet>,
    parent: Option<&RecursiveParentState>,
    depth: usize,
) -> Result<(uqa_execution::SharedSpill, uqa_execution::SharedSpill), SQLError> {
    let mut emitted = uqa_execution::SpillBuffer::new(1);
    let mut expandable = uqa_execution::SpillBuffer::new(1);
    for batch in base_rows.reader().map_err(physical_exec_error)? {
        let batch = batch.map_err(physical_exec_error)?;
        let capacity = batch.rows.len().min(uqa_execution::DEFAULT_BATCH_SIZE);
        let mut emitted_rows = Vec::with_capacity(capacity);
        let mut expandable_rows = Vec::with_capacity(capacity);
        for row in batch.rows {
            let base = uqa_execution::OwnedPhysicalRow::new(batch.schema.clone(), row);
            let (row, is_cycle) = annotate_recursive_row(
                &base,
                cte,
                search_positions,
                cycle_positions,
                cycle_marks,
                parent,
                depth,
            )?;
            collect_annotated_recursive_row(
                row,
                is_cycle,
                generated_schema,
                &mut seen,
                &mut emitted_rows,
                &mut expandable_rows,
            )?;
        }
        push_annotated_recursive_batch(
            generated_schema,
            emitted_rows,
            expandable_rows,
            &mut emitted,
            &mut expandable,
        )?;
    }
    Ok((
        emitted
            .into_shared(generated_schema.clone())
            .map_err(physical_exec_error)?,
        expandable
            .into_shared(generated_schema.clone())
            .map_err(physical_exec_error)?,
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps SELECT scope inputs aligned"
)]
fn annotate_recursive_step_rows(
    step_rows: &uqa_execution::SharedSpill,
    base_schema: &uqa_execution::RowSchema,
    generated_schema: &uqa_execution::RowSchema,
    cte: &CtePlan,
    search_positions: Option<&[usize]>,
    cycle_positions: Option<&[usize]>,
    cycle_marks: Option<&(Value, Value)>,
    mut seen: Option<&mut uqa_execution::ExactRowSet>,
    controlled_step: &ControlledRecursiveStep,
    depth: usize,
) -> Result<(uqa_execution::SharedSpill, uqa_execution::SharedSpill), SQLError> {
    let lineage_width =
        usize::from(controlled_step.parent_search) + usize::from(controlled_step.parent_cycle_path);
    let expected_width = base_schema.len() + lineage_width;
    if step_rows.row_schema().len() != expected_width {
        return Err(SQLError::TypeMismatch(format!(
            "recursive term has {} columns but expected {expected_width}",
            step_rows.row_schema().len()
        )));
    }
    let mut emitted = uqa_execution::SpillBuffer::new(1);
    let mut expandable = uqa_execution::SpillBuffer::new(1);
    for batch in step_rows.reader().map_err(physical_exec_error)? {
        let batch = batch.map_err(physical_exec_error)?;
        let capacity = batch.rows.len().min(uqa_execution::DEFAULT_BATCH_SIZE);
        let mut emitted_rows = Vec::with_capacity(capacity);
        let mut expandable_rows = Vec::with_capacity(capacity);
        for row in batch.rows {
            let source = uqa_execution::OwnedPhysicalRow::new(batch.schema.clone(), row);
            let base_values = (0..base_schema.len())
                .map(|position| {
                    source.view().value_at(position).cloned().ok_or_else(|| {
                        SQLError::Internal(format!(
                            "recursive term is missing base column at position {position}"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let base = uqa_execution::OwnedPhysicalRow::new(
                base_schema.clone(),
                uqa_execution::PhysicalRow::from_values(base_values),
            );
            let mut lineage_position = base_schema.len();
            let search_sequence = if controlled_step.parent_search {
                let value = source
                    .view()
                    .value_at(lineage_position)
                    .cloned()
                    .ok_or_else(|| {
                        SQLError::Internal("recursive term lost SEARCH parent state".into())
                    })?;
                lineage_position += 1;
                Some(value)
            } else {
                None
            };
            let cycle_path = if controlled_step.parent_cycle_path {
                Some(
                    source
                        .view()
                        .value_at(lineage_position)
                        .cloned()
                        .ok_or_else(|| {
                            SQLError::Internal("recursive term lost CYCLE parent state".into())
                        })?,
                )
            } else {
                None
            };
            let parent = RecursiveParentState {
                search_sequence,
                cycle_path,
            };
            let (row, is_cycle) = annotate_recursive_row(
                &base,
                cte,
                search_positions,
                cycle_positions,
                cycle_marks,
                Some(&parent),
                depth,
            )?;
            collect_annotated_recursive_row(
                row,
                is_cycle,
                generated_schema,
                &mut seen,
                &mut emitted_rows,
                &mut expandable_rows,
            )?;
        }
        push_annotated_recursive_batch(
            generated_schema,
            emitted_rows,
            expandable_rows,
            &mut emitted,
            &mut expandable,
        )?;
    }
    Ok((
        emitted
            .into_shared(generated_schema.clone())
            .map_err(physical_exec_error)?,
        expandable
            .into_shared(generated_schema.clone())
            .map_err(physical_exec_error)?,
    ))
}

fn annotate_recursive_row(
    base: &uqa_execution::OwnedPhysicalRow,
    cte: &CtePlan,
    search_positions: Option<&[usize]>,
    cycle_positions: Option<&[usize]>,
    cycle_marks: Option<&(Value, Value)>,
    parent: Option<&RecursiveParentState>,
    depth: usize,
) -> Result<(uqa_execution::PhysicalRow, bool), SQLError> {
    let mut values = base
        .view()
        .iter()
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>();
    if let (Some(search), Some(positions)) = (&cte.search, search_positions) {
        let key = positions
            .iter()
            .map(|position| {
                base.view()
                    .value_at(*position)
                    .cloned()
                    .unwrap_or(Value::Null)
            })
            .collect::<Vec<_>>();
        let sequence = if search.breadth_first {
            let mut fields = Vec::with_capacity(key.len() + 1);
            fields.push(Value::Int(i64::try_from(depth).map_err(|_| {
                SQLError::Unsupported("recursive CTE depth exceeds bigint".into())
            })?));
            fields.extend(key);
            Value::Row(fields)
        } else {
            let mut path = parent
                .and_then(|parent| parent.search_sequence.as_ref())
                .and_then(|value| match value {
                    Value::Array(array) => Some(array.elements().to_vec()),
                    _ => None,
                })
                .unwrap_or_default();
            path.push(Value::Row(key));
            Value::Array(ArrayValue::try_new(path).ok_or_else(|| {
                SQLError::Internal("SEARCH path is not a rectangular SQL array".into())
            })?)
        };
        values.push(sequence);
    }
    let mut is_cycle = false;
    if let (Some(_cycle), Some(positions), Some((mark, default))) =
        (&cte.cycle, cycle_positions, cycle_marks)
    {
        let key = Value::Row(
            positions
                .iter()
                .map(|position| {
                    base.view()
                        .value_at(*position)
                        .cloned()
                        .unwrap_or(Value::Null)
                })
                .collect(),
        );
        let mut path = parent
            .and_then(|parent| parent.cycle_path.as_ref())
            .and_then(|value| match value {
                Value::Array(array) => Some(array.elements().to_vec()),
                _ => None,
            })
            .unwrap_or_default();
        is_cycle = path.iter().any(|existing| existing == &key);
        path.push(key);
        values.push(if is_cycle {
            mark.clone()
        } else {
            default.clone()
        });
        values.push(Value::Array(ArrayValue::try_new(path).ok_or_else(
            || SQLError::Internal("CYCLE path is not a rectangular SQL array".into()),
        )?));
    }
    Ok((uqa_execution::PhysicalRow::from_values(values), is_cycle))
}

fn collect_annotated_recursive_row(
    row: uqa_execution::PhysicalRow,
    is_cycle: bool,
    generated_schema: &uqa_execution::RowSchema,
    seen: &mut Option<&mut uqa_execution::ExactRowSet>,
    emitted: &mut Vec<uqa_execution::PhysicalRow>,
    expandable: &mut Vec<uqa_execution::PhysicalRow>,
) -> Result<(), SQLError> {
    let is_new = match seen.as_deref_mut() {
        Some(seen) => seen
            .insert_physical(&row, generated_schema)
            .map_err(physical_exec_error)?,
        None => true,
    };
    if !is_new {
        return Ok(());
    }
    emitted.push(row.clone());
    if !is_cycle {
        expandable.push(row);
    }
    Ok(())
}

fn push_annotated_recursive_batch(
    generated_schema: &uqa_execution::RowSchema,
    emitted_rows: Vec<uqa_execution::PhysicalRow>,
    expandable_rows: Vec<uqa_execution::PhysicalRow>,
    emitted: &mut uqa_execution::SpillBuffer,
    expandable: &mut uqa_execution::SpillBuffer,
) -> Result<(), SQLError> {
    if !emitted_rows.is_empty() {
        emitted
            .push(uqa_execution::Batch::from_physical_rows(
                generated_schema.clone(),
                emitted_rows,
            ))
            .map_err(physical_exec_error)?;
    }
    if !expandable_rows.is_empty() {
        expandable
            .push(uqa_execution::Batch::from_physical_rows(
                generated_schema.clone(),
                expandable_rows,
            ))
            .map_err(physical_exec_error)?;
    }
    Ok(())
}

pub(in crate::sql) fn alias_query_output_to_shared(
    engine: &Engine,
    output: QueryOutput,
    aliases: &[String],
) -> Result<uqa_execution::SharedSpill, SQLError> {
    let visible_source_columns = output.columns.clone();
    let source_columns = output.internal_columns.clone();
    let columns = visible_source_columns
        .iter()
        .enumerate()
        .map(|(index, source)| {
            aliases
                .get(index)
                .cloned()
                .unwrap_or_else(|| source.clone())
        })
        .collect::<Vec<_>>();
    let mut operator = output.into_operator();
    if source_columns != columns {
        let mapping = source_columns
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let output = columns
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| source.clone());
                (output, index)
            })
            .collect::<Vec<_>>();
        operator = Box::new(uqa_execution::ColumnSelection::with_positions(
            operator, mapping,
        ));
    }
    let identity = operator
        .row_schema()
        .columns()
        .iter()
        .cloned()
        .enumerate()
        .map(|(position, column)| (column, position))
        .collect();
    operator = Box::new(uqa_execution::ColumnSelection::compacting_with_positions(
        operator, identity,
    ));
    let output = collect_query_operator(engine, columns, operator, QueryOutputMode::SharedSpill)?;
    let QueryRows::SharedSpill(rows) = output.rows else {
        return Err(SQLError::Internal(
            "CTE collector returned in-memory rows".into(),
        ));
    };
    Ok(rows)
}

pub(in crate::sql) fn append_shared_spill(
    output: &mut uqa_execution::SpillBuffer,
    rows: &uqa_execution::SharedSpill,
) -> Result<(), SQLError> {
    let reader = rows.reader().map_err(physical_exec_error)?;
    for batch in reader {
        output
            .push(batch.map_err(physical_exec_error)?)
            .map_err(physical_exec_error)?;
    }
    Ok(())
}

pub(in crate::sql) fn filter_new_recursive_rows(
    input: &uqa_execution::SharedSpill,
    seen: &mut uqa_execution::ExactRowSet,
) -> Result<uqa_execution::SharedSpill, SQLError> {
    // The source is already disk-backed. Retain no cardinality-sized tail
    // while constructing the next working set.
    let mut output = uqa_execution::SpillBuffer::new(1);
    let schema = input.row_schema().clone();
    let reader = input.reader().map_err(physical_exec_error)?;
    for batch in reader {
        let batch = batch.map_err(physical_exec_error)?;
        let mut rows = Vec::with_capacity(batch.rows.len().min(uqa_execution::DEFAULT_BATCH_SIZE));
        for row in batch.rows {
            if seen
                .insert_physical(&row, &batch.schema)
                .map_err(physical_exec_error)?
            {
                rows.push(row);
            }
        }
        if !rows.is_empty() {
            output
                .push(uqa_execution::Batch::from_physical_rows(
                    schema.clone(),
                    rows,
                ))
                .map_err(physical_exec_error)?;
        }
    }
    output.into_shared(schema).map_err(physical_exec_error)
}
