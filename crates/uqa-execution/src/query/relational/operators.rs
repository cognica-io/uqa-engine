//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational physical-operator assembly.

use super::aggregation::{
    append_distinct_set_projections, prepare_aggregate_key_statement, prepare_order_set_projections,
};
use super::filter::attach_relational_filter;
use super::limit::attach_order_limit;
use super::ordering::{
    append_row_at_time_projection, attach_final_projection_order, identity_order_columns,
    order_projection, output_selection_positions, resolve_order_expression,
    split_locking_order_projections, validate_distinct_ordering, FinalProjectionExecution,
};
use super::{build_set_projection, RelationalContext, RelationalResjunk};
use crate::aggregation::PhysicalAggregateExecutor;
use crate::query::projection::{
    expand_bound_projection_stars, physical_projections, physical_work_mem_bytes,
    projection_set_batch_size,
};
use crate::query::row_at_a_time::RowAtATime;
use crate::query::runtime::QueryRuntimeView;
use crate::query::CteScope;
use crate::{ColumnSelection, HashAggregate, PhysicalOperator, Project};
use std::sync::Arc;
use uqa_sql::plan::{ComputePlan, QueryBlockPlan};
use uqa_sql::semantics::sets::{
    rewrite::{prepare_aggregate_output_projection, prepare_group_set_projection},
    validation::{expression_may_return_set, projections_may_return_set},
};
use uqa_sql::semantics::{grouping_sets::prepare_grouping_sets, projection_columns};
use uqa_sql::{SQLError, SQLParam, ScalarExpr};

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub fn build_relational_operator<'a, S: Clone + 'static>(
    context: RelationalContext<'a, S>,
    operator: Box<dyn PhysicalOperator + 'a>,
    predicate: Option<ScalarExpr>,
    statement: &QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
    runtime: QueryRuntimeView<'a>,
) -> Result<(Box<dyn PhysicalOperator + 'a>, RelationalResjunk), SQLError> {
    crate::physical::construction::OperatorBuild::run(operator, |operator| {
        let resources = context.expressions.production_resources()?;
        let control = resources.control();
        let type_resolver = context.expression_scope(ctes)?;
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
        if statement.distinct {
            resjunk.constant_distinct_columns = statement
                .projections
                .iter()
                .enumerate()
                .filter_map(|(position, projection)| {
                    uqa_sql::type_resolution::comparison_key_is_constant(&projection.expr)
                        .then_some(position)
                })
                .collect();
        }
        validate_distinct_ordering(statement)?;
        if ctes.streams_command_progress() {
            operator.wrap(|operator| Box::new(RowAtATime::new(operator)));
        }
        if ctes.scans_backwards() && statement.from.is_some() {
            operator.wrap(|operator| crate::prepare_backward_scan(operator));
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
                    context.catalog,
                    type_resolver.as_ref(),
                    &expression,
                    operator.row_schema(),
                    params,
                )
            })?;
            if projections_may_return_set(
                context.catalog,
                type_resolver.as_ref(),
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
        let evaluator = context.evaluator(params, ctes)?;
        operator.attach(|operator| {
            attach_relational_filter(context, operator, predicate, params, ctes, &evaluator)
        })?;
        let bound_group_statement = if matches!(statement.compute, ComputePlan::Aggregate) {
            prepare_grouping_sets(context.catalog, statement, operator.row_schema(), params)?
        } else {
            None
        };
        let statement = bound_group_statement.as_ref().unwrap_or(statement);
        let aggregate_outer = if matches!(statement.compute, ComputePlan::Aggregate) {
            ctes.row_lock_outer_row()
                .map(|outer| {
                    crate::aggregation::AggregateOuterRow::new(
                        outer,
                        operator.row_schema().physical_width(),
                    )
                })
                .transpose()?
        } else {
            None
        };
        let mut group_statement = None;
        if matches!(statement.compute, ComputePlan::Aggregate) {
            if let Some(plan) = prepare_group_set_projection(
                context.catalog,
                type_resolver.as_ref(),
                statement,
                operator.row_schema(),
                params,
            )? {
                operator.attach(|operator| {
                    build_set_projection(
                        operator,
                        context,
                        params,
                        ctes,
                        Arc::clone(&evaluator),
                        crate::query::set_projection::SetProjectionOutput {
                            projections: plan.projections,
                            pass_through: true,
                            batch_size: crate::DEFAULT_BATCH_SIZE,
                        },
                    )
                })?;
                group_statement = Some(plan.statement);
            }
        }
        let statement = group_statement.as_ref().unwrap_or(statement);

        match statement.compute {
            ComputePlan::Project => {
                if statement.order_by.is_empty() {
                    let distinct_output =
                        identity_order_columns(&projection_columns(&statement.projections));
                    let mut distinct_inputs = uqa_core::memory::ProductionVec::new(control);
                    super::aggregation::visit_distinct_set_projections(
                        statement,
                        &distinct_output,
                        |index, column, expression| {
                            distinct_inputs.push_copy((index, column, expression))?;
                            Ok(())
                        },
                    )?;
                    resjunk.distinct_on.extend(
                        distinct_inputs
                            .iter()
                            .map(|(index, column, _)| (*index, *column)),
                    );
                    if !statement.locking.is_empty() {
                        let (physical, output) =
                            order_projection(&statement.projections, operator.row_schema())?;
                        let recheck_projections = physical.clone();
                        operator.wrap(|input| {
                            append_row_at_time_projection(input, physical, Arc::clone(&evaluator))
                        });
                        operator.attach(|operator| {
                            attach_order_limit(
                        operator,
                        statement,
                        &[],
                        context,
                        params,
                        ctes,
                        runtime,
                        Arc::clone(&evaluator),
                        Some(
                            crate::query::recheck_source::LockRowsRecheckInput::with_projections(
                                statement,
                                ctes,
                                false,
                                recheck_projections,
                                &control,
                            ),
                        ),
                    )
                        })?;
                        let output = output_selection_positions(operator.row_schema(), output)?;
                        operator.wrap(|operator| {
                            Box::new(ColumnSelection::with_physical_positions(operator, output))
                        });
                    } else if uqa_sql::semantics::sets::validation::expressions_may_return_set_with_control(
                        context.catalog, type_resolver.as_ref(),
                        statement.projections.iter().map(|projection| &projection.expr).chain(distinct_inputs.iter().map(|(_, _, expression)| *expression)),
                        operator.row_schema(), params, &control,
                    )? {
                        let mut projections = physical_projections(&statement.projections);
                        projections.extend(distinct_inputs.iter().map(|(_, column, expression)| (crate::ProjectionTarget::Internal(*column), (*expression).clone())));
                        // PostgreSQL evaluates set-returning target expressions above LockRows, so a rechecked tuple re-expands its rows from the substituted base tuple. Locks therefore attach below the set projection, and LIMIT is applied to the expanded output above.
                        operator.attach(|operator| {
                            super::attach_lock_rows(
                                context,
                                operator,
                                statement,
                                params,
                                ctes,
                                None,
                                Some(crate::query::recheck_source::LockRowsRecheckInput::new(
                                    statement, ctes, false, &control,
                                )),
                            )
                        })?;
                        operator.attach(|operator| {
                            build_set_projection(
                                operator,
                                context,
                                params,
                                ctes,
                                Arc::clone(&evaluator),
                                crate::query::set_projection::SetProjectionOutput {
                                    projections,
                                    pass_through: false,
                                    batch_size: projection_set_batch_size(statement, ctes),
                                },
                            )
                        })?;
                        let unlocked_statement = (!statement.locking.is_empty()).then(|| {
                            let mut unlocked = statement.clone();
                            unlocked.locking.clear();
                            unlocked
                        });
                        operator.attach(|operator| {
                            attach_order_limit(
                                operator,
                                unlocked_statement.as_ref().unwrap_or(statement),
                                &[],
                                context,
                                params,
                                ctes,
                                runtime,
                                evaluator,
                                None,
                            )
                        })?;
                    } else {
                        // PostgreSQL evaluates the target list before OFFSET discards rows, while LIMIT must still stop before the next unused target row is evaluated. A one-row pull boundary preserves both requirements.
                        if statement.limit.is_some() || statement.offset.is_some() {
                            operator.wrap(|operator| Box::new(RowAtATime::new(operator)));
                        }
                        let work_mem = runtime.work_mem_bytes()?;
                        let labels = uqa_sql::semantics::projection_labels_with_control(&statement.projections, &[], &control)?;
                        operator.attach(|operator| {
                            let projections = labels.iter().zip(&statement.projections).map(|(label, projection)| {
                                crate::query::projection::owned_physical_projection(crate::query::projection::owned_projection_column(label, &control)?, &projection.expr, true, &control)
                            }).chain(distinct_inputs.iter().map(|(_, column, expression)| {
                                crate::query::projection::owned_physical_projection(control.finish(crate::ProjectionTarget::Internal(*column), control.empty_reservation())?, expression, false, &control)
                            }));
                            let project = Project::with_owned_target_evaluator(operator, projections, Arc::clone(&evaluator), &control)?;
                            Ok(Box::new(project.with_memory_budget(work_mem)))
                        })?;
                        operator.attach(|operator| {
                            attach_order_limit(
                                operator,
                                statement,
                                &[],
                                context,
                                params,
                                ctes,
                                runtime,
                                Arc::clone(&evaluator),
                                Some(crate::query::recheck_source::LockRowsRecheckInput::new(
                                    statement, ctes, false, &control,
                                )),
                            )
                        })?;
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
                        context.catalog,
                        type_resolver.as_ref(),
                        statement,
                        &order_output,
                        &mut physical,
                        operator.row_schema(),
                        params,
                    )?;
                    resjunk.order_by.extend(order_columns);
                    if statement.locking.is_empty() && !ctes.streams_command_progress() {
                        if projections_may_return_set(
                            context.catalog,
                            type_resolver.as_ref(),
                            &physical,
                            operator.row_schema(),
                            params,
                        )? {
                            operator.attach(|operator| {
                                build_set_projection(
                                    operator,
                                    context,
                                    params,
                                    ctes,
                                    Arc::clone(&evaluator),
                                    crate::query::set_projection::SetProjectionOutput {
                                        projections: physical,
                                        pass_through: true,
                                        batch_size: crate::DEFAULT_BATCH_SIZE,
                                    },
                                )
                            })?;
                        } else {
                            let work_mem = runtime.work_mem_bytes()?;
                            operator.wrap(|operator| {
                                Box::new(
                                    Project::appending_target_evaluator(
                                        operator,
                                        physical,
                                        Arc::clone(&evaluator),
                                    )
                                    .with_memory_budget(work_mem),
                                )
                            });
                        }
                        operator.attach(|operator| {
                            attach_order_limit(
                                operator,
                                order_statement.as_ref().unwrap_or(statement),
                                &order_output,
                                context,
                                params,
                                ctes,
                                runtime,
                                evaluator,
                                None,
                            )
                        })?;
                    } else if statement.locking.is_empty() {
                        operator.attach(|operator| {
                            super::ordering::attach_streaming_order_projection(
                                operator,
                                order_statement.as_ref().unwrap_or(statement),
                                &order_output,
                                physical,
                                FinalProjectionExecution {
                                    context,
                                    params,
                                    ctes,
                                    runtime,
                                    evaluator: Arc::clone(&evaluator),
                                },
                            )
                        })?;
                    } else {
                        let effective_order_statement =
                            order_statement.as_ref().unwrap_or(statement);
                        let recheck_projections = physical.clone();
                        let (mut sort_statement, before_sort, after_sort) =
                            split_locking_order_projections(
                                effective_order_statement,
                                &order_output,
                                physical,
                            )?;
                        if !before_sort.is_empty() {
                            let work_mem = runtime.work_mem_bytes()?;
                            operator.wrap(|operator| {
                                Box::new(
                                    Project::appending_target_evaluator(
                                        operator,
                                        before_sort,
                                        Arc::clone(&evaluator),
                                    )
                                    .with_memory_budget(work_mem),
                                )
                            });
                        }
                        sort_statement.locking.clear();
                        sort_statement.limit = None;
                        sort_statement.with_ties = false;
                        sort_statement.offset = None;
                        operator.attach(|operator| {
                            attach_order_limit(
                                operator,
                                &sort_statement,
                                &order_output,
                                context,
                                params,
                                ctes,
                                runtime,
                                Arc::clone(&evaluator),
                                None,
                            )
                        })?;
                        operator.wrap(|input| {
                            append_row_at_time_projection(input, after_sort, Arc::clone(&evaluator))
                        });
                        let mut lock_statement = statement.clone();
                        lock_statement.order_by = sort_statement.order_by;
                        operator.attach(|operator| {
                            attach_order_limit(
                        operator,
                        &lock_statement,
                        &order_output,
                        context,
                        params,
                        ctes,
                        runtime,
                        Arc::clone(&evaluator),
                        Some(
                            crate::query::recheck_source::LockRowsRecheckInput::with_projections(
                                statement,
                                ctes,
                                true,
                                recheck_projections,
                                &control,
                            ),
                        ),
                    )
                        })?;
                    }
                    let output = output_selection_positions(operator.row_schema(), output)?;
                    operator.wrap(|operator| {
                        Box::new(ColumnSelection::with_physical_positions(operator, output))
                    });
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
                        crate::require_equality_operator(&ty)?;
                    }
                }
                let work_mem_bytes = physical_work_mem_bytes(runtime)?;
                let output_plan = prepare_aggregate_output_projection(
                    context.catalog,
                    statement,
                    internal_targets,
                );
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
                let aggregate_row_schema =
                    crate::RowSchema::with_types(aggregate_schema.clone(), aggregate_types.clone());
                let aggregate_executor = PhysicalAggregateExecutor::with_outer_scope(
                    context.expression_scope(ctes)?,
                    &output_plan.statement,
                    params,
                    input_schema,
                    aggregate_row_schema,
                    work_mem_bytes,
                    aggregate_outer,
                )?;
                operator.wrap(|operator| {
                    Box::new(HashAggregate::with_typed_executor(
                        operator,
                        aggregate_schema,
                        aggregate_types,
                        Box::new(aggregate_executor),
                    ))
                });
                let output = identity_order_columns(&schema);
                operator.attach(|operator| {
                    attach_final_projection_order(
                        operator,
                        (statement, &output),
                        output_plan.projections,
                        FinalProjectionExecution {
                            context,
                            params,
                            ctes,
                            runtime,
                            evaluator,
                        },
                    )
                })?;
            }
            ComputePlan::Window => {
                operator.attach(|operator| {
                    super::window_output::attach_window_output(
                        operator,
                        statement,
                        &mut resjunk,
                        type_resolver.as_ref(),
                        FinalProjectionExecution {
                            context,
                            params,
                            ctes,
                            runtime,
                            evaluator,
                        },
                    )
                })?;
            }
        }

        Ok(resjunk)
    })
}
