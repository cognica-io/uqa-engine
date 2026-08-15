//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recursive CTE materialization and spill deduplication.

use super::{
    attach_order_limit, collect_query_operator, execute_query_plan_output, identity_order_columns,
    is_score_provenance_column, materialize_plan_ctes, physical_exec_error,
    physical_work_mem_bytes, push_output_filter_into_query_plan, query_plan_output_columns,
    AccessPathPlan, ComputePlan, CtePlan, CteScope, Engine, EngineExpressionEvaluator,
    QueryBlockPlan, QueryOutput, QueryOutputMode, QueryRows, RelationalPlan, SQLError, SQLParam,
    ScalarExpr, SetOpKind,
};

/// Iterate the recursive `CtePlan`: take the anchor (LHS of UNION ALL) as
/// the initial row set, then repeatedly evaluate the recursive step
/// (RHS) with the `CtePlan` bound to the *new rows from the previous
/// iteration* (working set), unioning the result back into the total.
/// Caps at 1024 iterations to keep buggy queries from running away.
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
        subqueries,
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

    let declared_columns = (!cte.columns.is_empty()).then_some(cte.columns.as_slice());
    let (anchor_plan, step_plan) = if let Some((qualifier, filter)) = output_filter {
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

    let work_mem = physical_work_mem_bytes(engine)?.max(1);
    // The accumulated rows and UNION duplicate state are live together. Give
    // each at most half of work_mem; SharedSpill working sets are disk-only.
    let state_budget = (work_mem / 2).max(1);
    let mut accumulated = uqa_execution::SpillBuffer::new(state_budget);
    let mut seen = (!*all).then(|| uqa_execution::ExactRowSet::new(state_budget));
    if let Some(seen) = seen.as_mut() {
        working = filter_new_recursive_rows(&working, &anchor_columns, seen)?;
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
            working = filter_new_recursive_rows(&working, &anchor_columns, seen)?;
        }
    }

    let rows = accumulated
        .into_shared(anchor_schema)
        .map_err(physical_exec_error)?;

    if order_by.is_empty() && limit.is_none() && offset.is_none() {
        return Ok(rows);
    }
    let synthetic = QueryBlockPlan {
        projections: Vec::new(),
        from: None,
        r#where: None,
        compute: ComputePlan::Project,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        having: None,
        order_by: order_by.clone(),
        limit: limit.as_deref().cloned(),
        offset: offset.as_deref().cloned(),
        distinct: false,
        distinct_on: Vec::new(),
        subqueries: subqueries.clone(),
        access: AccessPathPlan::Row,
    };
    let ordering_scope = ctes.enter_scalar_subqueries(subqueries);
    let operation: Box<dyn uqa_execution::PhysicalOperator + '_> =
        Box::new(uqa_execution::SharedSpillScan::new(rows));
    let output = identity_order_columns(&anchor_columns);
    let operation = attach_order_limit(
        operation,
        &synthetic,
        &output,
        engine,
        params,
        &ordering_scope,
        EngineExpressionEvaluator::shared(engine, params, &ordering_scope),
    )?;
    let output = collect_query_operator(
        engine,
        anchor_columns,
        operation,
        QueryOutputMode::SharedSpill,
    )?;
    let QueryRows::SharedSpill(rows) = output.rows else {
        return Err(SQLError::Internal(
            "recursive CTE collector returned in-memory rows".into(),
        ));
    };
    Ok(rows)
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
                let output = if is_score_provenance_column(source) {
                    source.clone()
                } else {
                    columns
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| source.clone())
                };
                (output, index)
            })
            .collect::<Vec<_>>();
        operator = Box::new(uqa_execution::ColumnSelection::with_positions(
            operator, mapping,
        ));
    }
    let output = collect_query_operator(engine, columns, operator, QueryOutputMode::SharedSpill)?;
    let QueryRows::SharedSpill(rows) = output.rows else {
        return Err(SQLError::Internal(
            "recursive term collector returned in-memory rows".into(),
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
    columns: &[String],
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
            let result_row = batch.schema.view(&row).to_result_row();
            if seen
                .insert_row(&result_row, columns)
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
