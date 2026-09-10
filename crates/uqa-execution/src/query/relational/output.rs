//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical filtering, DISTINCT, output limits, and result finishing.

use super::{
    filter::attach_relational_filter,
    limit::resolved_sort_keys,
    operators::build_relational_operator,
    ordering::{distinct_output_target_position, identity_order_columns, prior_distinct_key_index},
    row_count::{resolve_fetch_limit_with_ties, resolve_limit_offset_with_ctes},
    RelationalContext, RelationalResjunk,
};
use crate::query::{
    collection::collect_query_operator,
    consumer::QueryOutputMode,
    output::{collect_exists_key_operator, QueryOutput},
    projection::{physical_exec_error, physical_projections, physical_work_mem_bytes},
    runtime::QueryRuntimeView,
    CteScope,
};
use crate::{
    physical::run_to_batches, scan::TableScan, ColumnSelection, Distinct, Filter, Limit,
    PhysicalOperator, ScalarExpr,
};
use uqa_sql::{
    plan::{ComputePlan, QueryBlockPlan},
    semantics::{sets::validation::projections_may_return_set, should_defer_distinct_limit},
    SQLError, SQLParam,
};

pub fn execute_filter_physical_rows<S: Clone + 'static>(
    context: RelationalContext<'_, S>,
    schema: crate::RowSchema,
    rows: Vec<crate::PhysicalRow>,
    predicate: ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Vec<crate::OwnedPhysicalRow>, SQLError> {
    let scan: Box<dyn PhysicalOperator + '_> =
        Box::new(TableScan::from_physical_rows(schema, rows));
    let evaluator = context.evaluator(params, ctes);
    let mut filter = Filter::with_evaluator(scan, predicate, evaluator);
    Ok(run_to_batches(&mut filter)
        .map_err(physical_exec_error)?
        .into_iter()
        .flat_map(crate::Batch::into_owned_rows)
        .collect())
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps SELECT scope inputs aligned"
)]
pub fn execute_query_block_operator_output<'a, S: Clone + 'static>(
    context: RelationalContext<'a, S>,
    operator: Box<dyn PhysicalOperator + 'a>,
    predicate: Option<ScalarExpr>,
    statement: &'a QueryBlockPlan,
    original: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &'a CteScope<S>,
    columns: Vec<String>,
    output_mode: QueryOutputMode<'_>,
) -> Result<QueryOutput, SQLError> {
    let runtime = context.runtime;
    let type_resolver = context.expression_scope(ctes.clone());
    if matches!(&output_mode, QueryOutputMode::ExistsKeySet)
        && matches!(statement.compute, ComputePlan::Project)
        && statement.order_by.is_empty()
        && statement.limit.is_none()
        && statement.offset.is_none()
        && !statement.distinct
        && statement.distinct_on.is_empty()
        && !projections_may_return_set(
            context.catalog,
            type_resolver.as_ref(),
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
        let evaluator = context.evaluator(params, ctes);
        let operator =
            attach_relational_filter(context, operator, predicate, params, ctes, &evaluator)?;
        return collect_exists_key_operator(columns, operator, &statement.projections, evaluator);
    }
    let (operator, resjunk) = build_relational_operator(
        context, operator, predicate, statement, params, ctes, runtime,
    )?;
    finish_query_block_operator_output(
        context,
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

#[expect(
    clippy::too_many_arguments,
    reason = "keeps SELECT scope inputs aligned"
)]
#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub(super) fn finish_query_block_operator_output<'a, S: Clone + 'static>(
    context: RelationalContext<'a, S>,
    mut operator: Box<dyn PhysicalOperator + 'a>,
    original: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &'a CteScope<S>,
    columns: Vec<String>,
    output_mode: QueryOutputMode<'_>,
    resjunk: RelationalResjunk,
    runtime: QueryRuntimeView<'a>,
) -> Result<QueryOutput, SQLError> {
    if original.distinct {
        let work_mem_bytes = physical_work_mem_bytes(runtime)?;
        operator = if original.distinct_on.is_empty() {
            for position in 0..columns.len() {
                if let Some(ty) = operator.row_schema().column_type(position) {
                    crate::require_equality_operator(ty)?;
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
                if let Some(ty) = crate::scalar_type(expression, operator.row_schema(), params)? {
                    crate::require_equality_operator(&ty)?;
                }
            }
            Box::new(Distinct::on_with_work_mem(
                operator,
                distinct_on,
                context.evaluator(params, ctes),
                work_mem_bytes,
            ))
        };
    }
    if should_defer_distinct_limit(original) {
        let offset = resolve_limit_offset_with_ctes(
            original.offset.as_ref(),
            context,
            params,
            "OFFSET",
            ctes,
        )?;
        if original.with_ties {
            let limit =
                resolve_fetch_limit_with_ties(original.limit.as_ref(), context, params, ctes)?;
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
                context.evaluator(params, ctes),
            ));
        } else {
            let limit = resolve_limit_offset_with_ctes(
                original.limit.as_ref(),
                context,
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
    collect_query_operator(context.runtime, columns, operator, output_mode)
}
