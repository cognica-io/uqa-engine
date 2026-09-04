//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical filtering, result finishing, and operator collection.

use std::sync::Arc;

use uqa_execution::{
    physical::run_to_batches, scan::TableScan, ColumnSelection, Distinct, Filter, Limit,
    PhysicalOperator,
};
use uqa_planner::{ComputePlan, QueryBlockPlan};
use uqa_sql::{SQLError, SQLParam};

use crate::engine_capabilities::QueryRuntimeView;

use super::super::{
    collect_exists_key_operator, collect_query_operator, prepare_correlated_exists_predicate,
    projections_may_return_set, resolve_fetch_limit_with_ties, resolve_limit_offset_with_ctes,
    should_defer_distinct_limit, CteScope, Engine, EngineExpressionEvaluator, QueryOutput,
    QueryOutputMode, ScalarExpr, ScopedEngineHook, SharedExpressionEvaluator,
};
use super::limit::resolved_sort_keys;
use super::operators::build_relational_operator;
use super::ordering::{
    distinct_output_target_position, identity_order_columns, prior_distinct_key_index,
};
use super::projection::{physical_exec_error, physical_projections, physical_work_mem_bytes};
use super::RelationalResjunk;

pub(in crate::sql) fn execute_filter_physical_rows(
    engine: &Engine,
    schema: uqa_execution::RowSchema,
    rows: Vec<uqa_execution::PhysicalRow>,
    predicate: ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<uqa_execution::OwnedPhysicalRow>, SQLError> {
    let scan: Box<dyn PhysicalOperator + '_> =
        Box::new(TableScan::from_physical_rows(schema, rows));
    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    let mut filter = Filter::with_evaluator(scan, predicate, evaluator);
    Ok(run_to_batches(&mut filter)
        .map_err(physical_exec_error)?
        .into_iter()
        .flat_map(uqa_execution::Batch::into_owned_rows)
        .collect())
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps SELECT scope inputs aligned"
)]
pub(in crate::sql) fn execute_query_block_operator_output<'a>(
    engine: &'a Engine,
    operator: Box<dyn PhysicalOperator + 'a>,
    predicate: Option<ScalarExpr>,
    statement: &'a QueryBlockPlan,
    original: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
    columns: Vec<String>,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let runtime = engine.query_runtime_view();
    let type_resolver = ScopedEngineHook::new(engine, ctes);
    if matches!(&output_mode, QueryOutputMode::ExistsKeySet)
        && matches!(statement.compute, ComputePlan::Project)
        && statement.order_by.is_empty()
        && statement.limit.is_none()
        && statement.offset.is_none()
        && !statement.distinct
        && statement.distinct_on.is_empty()
        && !projections_may_return_set(
            engine,
            &type_resolver,
            &physical_projections(&statement.projections),
            operator.row_schema(),
            params,
        )?
        && matches!(original.compute, ComputePlan::Project)
        && original.order_by.is_empty()
        && original.limit.is_none()
        && original.offset.is_none()
        && !original.distinct
        && original.distinct_on.is_empty()
    {
        let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
        let operator =
            attach_relational_filter(engine, operator, predicate, params, ctes, &evaluator)?;
        return collect_exists_key_operator(columns, operator, &statement.projections, evaluator);
    }
    let (operator, resjunk) = build_relational_operator(
        engine, operator, predicate, statement, params, ctes, runtime,
    )?;
    finish_query_block_operator_output(
        engine,
        operator,
        original,
        params,
        ctes,
        columns,
        output_mode,
        resjunk,
        runtime,
    )
}

pub(super) fn attach_relational_filter<'a>(
    engine: &'a Engine,
    mut operator: Box<dyn PhysicalOperator + 'a>,
    predicate: Option<ScalarExpr>,
    params: &'a [SQLParam],
    ctes: &CteScope,
    evaluator: &SharedExpressionEvaluator<'a>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    if let Some(predicate) = predicate {
        operator = match prepare_correlated_exists_predicate(engine, &predicate, params, ctes)? {
            Some(prepared) => Box::new(Filter::with_row_predicate(operator, prepared)),
            None => Box::new(Filter::with_evaluator(
                operator,
                predicate,
                Arc::clone(evaluator),
            )),
        };
    }
    Ok(operator)
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps SELECT scope inputs aligned"
)]
#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub(super) fn finish_query_block_operator_output<'a>(
    engine: &'a Engine,
    mut operator: Box<dyn PhysicalOperator + 'a>,
    original: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
    columns: Vec<String>,
    output_mode: QueryOutputMode,
    resjunk: RelationalResjunk,
    runtime: QueryRuntimeView<'a>,
) -> Result<QueryOutput, SQLError> {
    if original.distinct {
        let work_mem_bytes = physical_work_mem_bytes(runtime)?;
        operator = if original.distinct_on.is_empty() {
            for position in 0..columns.len() {
                if let Some(ty) = operator.row_schema().column_type(position) {
                    uqa_execution::require_equality_operator(ty)?;
                }
            }
            Box::new(Distinct::all_with_work_mem(operator, work_mem_bytes))
        } else {
            let output = identity_order_columns(&columns);
            let mut distinct_on: Vec<ScalarExpr> = Vec::with_capacity(original.distinct_on.len());
            for (index, expression) in original.distinct_on.iter().enumerate() {
                let key = if let Some((_, column)) = resjunk
                    .distinct_on
                    .iter()
                    .find(|(key_index, _)| *key_index == index)
                {
                    ScalarExpr::InternalColumn(*column)
                } else if let Some(prior) =
                    prior_distinct_key_index(original, index, expression, &output)?
                {
                    distinct_on[prior].clone()
                } else if let Some(target) =
                    distinct_output_target_position(original, expression, &output)?
                {
                    ScalarExpr::Position(target.position)
                } else {
                    expression.clone()
                };
                distinct_on.push(key);
            }
            for expression in &distinct_on {
                if let Some(ty) =
                    uqa_execution::scalar_type(expression, operator.row_schema(), params)?
                {
                    uqa_execution::require_equality_operator(&ty)?;
                }
            }
            Box::new(Distinct::on_with_work_mem(
                operator,
                distinct_on,
                EngineExpressionEvaluator::shared(engine, params, ctes),
                work_mem_bytes,
            ))
        };
    }
    if should_defer_distinct_limit(original) {
        let offset = resolve_limit_offset_with_ctes(
            original.offset.as_ref(),
            engine,
            params,
            "OFFSET",
            ctes,
        )?;
        if original.with_ties {
            let limit =
                resolve_fetch_limit_with_ties(original.limit.as_ref(), engine, params, ctes)?;
            let output = identity_order_columns(&columns);
            let mut ordering = original.clone();
            for (index, column) in &resjunk.order_by {
                ordering.order_by[*index].expr = ScalarExpr::InternalColumn(*column);
            }
            let keys = resolved_sort_keys(&ordering, &output, Some(operator.row_schema()))?;
            operator = Box::new(Limit::with_ties(
                operator,
                offset.unwrap_or(0),
                limit,
                keys,
                EngineExpressionEvaluator::shared(engine, params, ctes),
            ));
        } else {
            let limit = resolve_limit_offset_with_ctes(
                original.limit.as_ref(),
                engine,
                params,
                "LIMIT",
                ctes,
            )?;
            operator = Box::new(Limit::new(operator, offset.unwrap_or(0), limit));
        }
    }
    let resjunk_columns = resjunk.columns();
    if !resjunk_columns.is_empty() {
        operator = Box::new(ColumnSelection::dropping_internal_attributes(
            operator,
            &resjunk_columns,
        ));
    }
    if operator.schema().len() < columns.len() {
        return Err(SQLError::Internal(format!(
            "query output schema width {} is smaller than public output width {}",
            operator.schema().len(),
            columns.len()
        )));
    }
    if operator.schema()[..columns.len()] != columns {
        let mut positions = columns
            .iter()
            .cloned()
            .enumerate()
            .map(|(position, output)| (output, position))
            .collect::<Vec<_>>();
        positions.extend(
            operator.schema()[columns.len()..]
                .iter()
                .cloned()
                .enumerate()
                .map(|(offset, output)| (output, columns.len() + offset)),
        );
        operator = Box::new(ColumnSelection::with_positions(operator, positions));
    }
    collect_query_operator(engine, columns, operator, output_mode)
}
