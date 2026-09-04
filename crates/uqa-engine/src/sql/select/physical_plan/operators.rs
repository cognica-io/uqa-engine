//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational physical-operator assembly.

use std::sync::Arc;

use uqa_execution::{ColumnSelection, HashAggregate, PhysicalOperator, Project, Window};
use uqa_planner::{ComputePlan, QueryBlockPlan};
use uqa_sql::{SQLError, SQLParam};

use crate::engine_capabilities::QueryRuntimeView;

use super::super::{
    build_set_projection, expression_may_return_set, prepare_aggregate_output_projection,
    prepare_distinct_grouping_sets, prepare_group_set_projection, prepare_window_plan,
    projection_columns, projections_may_return_set, CteScope, Engine, EngineExpressionEvaluator,
    PhysicalAggregateExecutor, PhysicalWindowExecutor, ScalarExpr, ScopedEngineHook,
};
use super::aggregation::{
    append_distinct_set_projections, prepare_aggregate_key_statement, prepare_order_set_projections,
};
use super::limit::attach_order_limit;
use super::ordering::{
    append_row_at_time_projection, attach_final_projection_order, identity_order_columns,
    order_projection, output_selection_positions, resolve_order_expression,
    split_locking_order_projections, validate_distinct_ordering, FinalProjectionExecution,
};
use super::output::attach_relational_filter;
use super::projection::{
    expand_bound_projection_stars, physical_projections, physical_work_mem_bytes,
    projection_set_batch_size,
};
use super::row_at_a_time::RowAtATime;
use super::RelationalResjunk;

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub(in crate::sql) fn build_relational_operator<'a>(
    engine: &'a Engine,
    mut operator: Box<dyn PhysicalOperator + 'a>,
    predicate: Option<ScalarExpr>,
    statement: &QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &CteScope,
    runtime: QueryRuntimeView<'a>,
) -> Result<(Box<dyn PhysicalOperator + 'a>, RelationalResjunk), SQLError> {
    let type_resolver = ScopedEngineHook::new(engine, ctes);
    let mut resjunk = RelationalResjunk::default();
    let expanded_statement = statement
        .projections
        .iter()
        .any(|projection| {
            matches!(
                projection.expr,
                ScalarExpr::Star | ScalarExpr::QualifiedStar(_)
            )
        })
        .then(|| {
            let mut expanded = statement.clone();
            expanded.projections =
                expand_bound_projection_stars(&statement.projections, operator.row_schema())?;
            Ok::<_, SQLError>(expanded)
        })
        .transpose()?;
    let statement = expanded_statement.as_ref().unwrap_or(statement);
    validate_distinct_ordering(statement)?;
    if ctes.streams_command_progress() {
        operator = Box::new(RowAtATime::new(operator));
    }
    if ctes.scans_backwards() && statement.from.is_some() {
        operator = uqa_execution::prepare_backward_scan(operator);
    }
    if let Some(clause) = statement.locking.first() {
        let projections = physical_projections(&statement.projections);
        let (_, output) = order_projection(&statement.projections, operator.row_schema())?;
        let order_returns_set = statement.order_by.iter().try_fold(false, |found, order| {
            if found {
                return Ok(true);
            }
            let expression = resolve_order_expression(&order.expr, &output)?;
            expression_may_return_set(
                engine,
                &type_resolver,
                &expression,
                operator.row_schema(),
                params,
            )
        })?;
        if projections_may_return_set(
            engine,
            &type_resolver,
            &projections,
            operator.row_schema(),
            params,
        )? || order_returns_set
        {
            return Err(SQLError::Unsupported(format!(
                "{} is not allowed with set-returning functions in the target list",
                clause.strength.sql_name()
            )));
        }
    }
    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    operator = attach_relational_filter(engine, operator, predicate, params, ctes, &evaluator)?;
    let distinct_group_statement = if matches!(statement.compute, ComputePlan::Aggregate) {
        prepare_distinct_grouping_sets(engine, statement, operator.row_schema(), params)?
    } else {
        None
    };
    let statement = distinct_group_statement.as_ref().unwrap_or(statement);
    let mut group_statement = None;
    if matches!(statement.compute, ComputePlan::Aggregate) {
        if let Some(plan) = prepare_group_set_projection(
            engine,
            &type_resolver,
            statement,
            operator.row_schema(),
            params,
        )? {
            operator = build_set_projection(
                operator,
                engine,
                params,
                ctes,
                Arc::clone(&evaluator),
                plan.projections,
                true,
                uqa_execution::DEFAULT_BATCH_SIZE,
            )?;
            group_statement = Some(plan.statement);
        }
    }
    let statement = group_statement.as_ref().unwrap_or(statement);

    match statement.compute {
        ComputePlan::Project => {
            if statement.order_by.is_empty() {
                let mut projections = physical_projections(&statement.projections);
                let distinct_output =
                    identity_order_columns(&projection_columns(&statement.projections));
                resjunk.distinct_on.extend(append_distinct_set_projections(
                    statement,
                    &distinct_output,
                    &mut projections,
                )?);
                if !statement.locking.is_empty() {
                    let (physical, output) =
                        order_projection(&statement.projections, operator.row_schema())?;
                    let recheck_projections = physical.clone();
                    operator =
                        append_row_at_time_projection(operator, physical, Arc::clone(&evaluator));
                    operator = attach_order_limit(
                        operator,
                        statement,
                        &[],
                        engine,
                        params,
                        ctes,
                        runtime,
                        Arc::clone(&evaluator),
                        Some(crate::sql::select::LockRowsRecheckSource::with_projections(
                            statement,
                            ctes,
                            false,
                            recheck_projections,
                        )),
                    )?;
                    let output = output_selection_positions(operator.row_schema(), output)?;
                    operator = Box::new(ColumnSelection::with_physical_positions(operator, output));
                } else if projections_may_return_set(
                    engine,
                    &type_resolver,
                    &projections,
                    operator.row_schema(),
                    params,
                )? {
                    // PostgreSQL evaluates set-returning target expressions above LockRows, so a rechecked tuple re-expands its rows from the substituted base tuple. Locks therefore attach below the set projection, and LIMIT is applied to the expanded output above.
                    operator = crate::sql::select::attach_lock_rows(
                        engine,
                        operator,
                        statement,
                        params,
                        ctes,
                        None,
                        Some(crate::sql::select::LockRowsRecheckSource::new(
                            statement, ctes, false,
                        )),
                    )?;
                    operator = build_set_projection(
                        operator,
                        engine,
                        params,
                        ctes,
                        Arc::clone(&evaluator),
                        projections,
                        false,
                        projection_set_batch_size(statement, ctes),
                    )?;
                    let unlocked_statement = (!statement.locking.is_empty()).then(|| {
                        let mut unlocked = statement.clone();
                        unlocked.locking.clear();
                        unlocked
                    });
                    operator = attach_order_limit(
                        operator,
                        unlocked_statement.as_ref().unwrap_or(statement),
                        &[],
                        engine,
                        params,
                        ctes,
                        runtime,
                        evaluator,
                        None,
                    )?;
                } else {
                    // PostgreSQL evaluates the target list before OFFSET discards rows, while LIMIT must still stop before the next unused target row is evaluated. A one-row pull boundary preserves both requirements.
                    if statement.limit.is_some() || statement.offset.is_some() {
                        operator = Box::new(RowAtATime::new(operator));
                    }
                    operator = Box::new(Project::with_target_evaluator(
                        operator,
                        projections,
                        Arc::clone(&evaluator),
                    ));
                    operator = attach_order_limit(
                        operator,
                        statement,
                        &[],
                        engine,
                        params,
                        ctes,
                        runtime,
                        Arc::clone(&evaluator),
                        Some(crate::sql::select::LockRowsRecheckSource::new(
                            statement, ctes, false,
                        )),
                    )?;
                }
            } else {
                let (mut physical, output) =
                    order_projection(&statement.projections, operator.row_schema())?;
                // SQL ordinals and aliases are resolved only against the visible SELECT list. Score provenance is carried through the final column selection for parent query blocks, but it is not itself a selectable output position.
                let order_output = output.clone();
                let distinct_columns =
                    append_distinct_set_projections(statement, &order_output, &mut physical)?;
                resjunk.distinct_on.extend(distinct_columns);
                let (order_statement, order_columns) = prepare_order_set_projections(
                    engine,
                    &type_resolver,
                    statement,
                    &order_output,
                    &mut physical,
                    operator.row_schema(),
                    params,
                )?;
                resjunk.order_by.extend(order_columns);
                if statement.locking.is_empty() && !ctes.streams_command_progress() {
                    operator = if projections_may_return_set(
                        engine,
                        &type_resolver,
                        &physical,
                        operator.row_schema(),
                        params,
                    )? {
                        build_set_projection(
                            operator,
                            engine,
                            params,
                            ctes,
                            Arc::clone(&evaluator),
                            physical,
                            true,
                            uqa_execution::DEFAULT_BATCH_SIZE,
                        )?
                    } else {
                        Box::new(Project::appending_target_evaluator(
                            operator,
                            physical,
                            Arc::clone(&evaluator),
                        ))
                    };
                    operator = attach_order_limit(
                        operator,
                        order_statement.as_ref().unwrap_or(statement),
                        &order_output,
                        engine,
                        params,
                        ctes,
                        runtime,
                        evaluator,
                        None,
                    )?;
                } else if statement.locking.is_empty() {
                    let effective_order_statement = order_statement.as_ref().unwrap_or(statement);
                    let (sort_statement, before_sort, after_sort) =
                        split_locking_order_projections(
                            effective_order_statement,
                            &order_output,
                            physical,
                        )?;
                    if !before_sort.is_empty() {
                        operator = Box::new(Project::appending_target_evaluator(
                            operator,
                            before_sort,
                            Arc::clone(&evaluator),
                        ));
                    }
                    if projections_may_return_set(
                        engine,
                        &type_resolver,
                        &after_sort,
                        operator.row_schema(),
                        params,
                    )? {
                        operator = build_set_projection(
                            operator,
                            engine,
                            params,
                            ctes,
                            Arc::clone(&evaluator),
                            after_sort,
                            true,
                            projection_set_batch_size(statement, ctes),
                        )?;
                        operator = attach_order_limit(
                            operator,
                            &sort_statement,
                            &order_output,
                            engine,
                            params,
                            ctes,
                            runtime,
                            Arc::clone(&evaluator),
                            None,
                        )?;
                    } else {
                        operator = attach_order_limit(
                            operator,
                            &sort_statement,
                            &order_output,
                            engine,
                            params,
                            ctes,
                            runtime,
                            Arc::clone(&evaluator),
                            None,
                        )?;
                        operator = append_row_at_time_projection(
                            operator,
                            after_sort,
                            Arc::clone(&evaluator),
                        );
                    }
                } else {
                    let effective_order_statement = order_statement.as_ref().unwrap_or(statement);
                    let recheck_projections = physical.clone();
                    let (mut sort_statement, before_sort, after_sort) =
                        split_locking_order_projections(
                            effective_order_statement,
                            &order_output,
                            physical,
                        )?;
                    if !before_sort.is_empty() {
                        operator = Box::new(Project::appending_target_evaluator(
                            operator,
                            before_sort,
                            Arc::clone(&evaluator),
                        ));
                    }
                    sort_statement.locking.clear();
                    sort_statement.limit = None;
                    sort_statement.with_ties = false;
                    sort_statement.offset = None;
                    operator = attach_order_limit(
                        operator,
                        &sort_statement,
                        &order_output,
                        engine,
                        params,
                        ctes,
                        runtime,
                        Arc::clone(&evaluator),
                        None,
                    )?;
                    operator =
                        append_row_at_time_projection(operator, after_sort, Arc::clone(&evaluator));
                    let mut lock_statement = statement.clone();
                    lock_statement.order_by = sort_statement.order_by;
                    operator = attach_order_limit(
                        operator,
                        &lock_statement,
                        &order_output,
                        engine,
                        params,
                        ctes,
                        runtime,
                        Arc::clone(&evaluator),
                        Some(crate::sql::select::LockRowsRecheckSource::with_projections(
                            statement,
                            ctes,
                            true,
                            recheck_projections,
                        )),
                    )?;
                }
                let output = output_selection_positions(operator.row_schema(), output)?;
                operator = Box::new(ColumnSelection::with_physical_positions(operator, output));
            }
        }
        ComputePlan::Aggregate => {
            let public_projection_count = statement.projections.len();
            let key_statement = prepare_aggregate_key_statement(statement)?;
            if let Some(keys) = &key_statement {
                resjunk.distinct_on.extend(keys.distinct_on.iter().copied());
                resjunk.order_by.extend(keys.order_by.iter().copied());
            }
            let internal_targets = key_statement
                .as_ref()
                .map_or(&[][..], |keys| keys.targets.as_slice());
            let statement = key_statement
                .as_ref()
                .map_or(statement, |keys| &keys.statement);
            let schema = projection_columns(&statement.projections[..public_projection_count]);
            let input_schema = operator.row_schema().clone();
            for expression in statement
                .group_by
                .iter()
                .chain(statement.grouping_sets.iter().flatten())
            {
                if let Some(ty) = evaluator.expression_type(expression, &input_schema)? {
                    uqa_execution::require_equality_operator(&ty)?;
                }
            }
            let work_mem_bytes = physical_work_mem_bytes(runtime)?;
            let output_plan =
                prepare_aggregate_output_projection(engine, statement, internal_targets);
            let aggregate_schema = projection_columns(&output_plan.statement.projections);
            let aggregate_types = output_plan
                .statement
                .projections
                .iter()
                .map(|projection| {
                    evaluator
                        .expression_type(&projection.expr, &input_schema)
                        .ok()
                        .flatten()
                })
                .collect::<Vec<_>>();
            let aggregate_row_schema = uqa_execution::RowSchema::with_types(
                aggregate_schema.clone(),
                aggregate_types.clone(),
            );
            let aggregate_executor = PhysicalAggregateExecutor::new(
                engine,
                &output_plan.statement,
                params,
                ctes,
                input_schema,
                aggregate_row_schema,
                work_mem_bytes,
            )?;
            operator = Box::new(HashAggregate::with_typed_executor(
                operator,
                aggregate_schema,
                aggregate_types,
                Box::new(aggregate_executor),
            ));
            let output = identity_order_columns(&schema);
            operator = attach_final_projection_order(
                operator,
                (statement, &output),
                output_plan.projections,
                FinalProjectionExecution {
                    engine,
                    params,
                    ctes,
                    runtime,
                    evaluator,
                },
            )?;
        }
        ComputePlan::Window => {
            let source_row_schema = operator.row_schema().clone();
            let work_mem_bytes = physical_work_mem_bytes(runtime)?;
            let window_plan = prepare_window_plan(&statement.projections);
            let mut projections = physical_projections(window_plan.projections());
            let schema = window_plan.output_schema(engine, &source_row_schema, params)?;
            let output_columns = order_projection(&statement.projections, &source_row_schema)?
                .1
                .into_iter()
                .enumerate()
                .map(|(position, (output, _))| (output, ScalarExpr::Position(position)))
                .collect::<Vec<_>>();
            resjunk.distinct_on.extend(append_distinct_set_projections(
                statement,
                &output_columns,
                &mut projections,
            )?);
            let (order_statement, order_columns) = prepare_order_set_projections(
                engine,
                &type_resolver,
                statement,
                &output_columns,
                &mut projections,
                &schema,
                params,
            )?;
            resjunk.order_by.extend(order_columns);
            operator = Box::new(Window::with_row_schema_executor(
                operator,
                schema.clone(),
                Box::new(PhysicalWindowExecutor::new(
                    engine,
                    window_plan,
                    params,
                    ctes,
                    source_row_schema,
                    work_mem_bytes,
                )),
            ));
            let effective_order_statement = order_statement.as_ref().unwrap_or(statement);
            operator = attach_final_projection_order(
                operator,
                (effective_order_statement, &output_columns),
                projections,
                FinalProjectionExecution {
                    engine,
                    params,
                    ctes,
                    runtime,
                    evaluator,
                },
            )?;
        }
    }

    Ok((operator, resjunk))
}
