//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTE scheduling, statement snapshot selection, and physical query execution.
use super::{
    bound_consumer::bind_output_mode,
    consumer::{QueryOutputMode, SetOperationConsumerFactory},
    context::{with_query_snapshot, QueryContext},
};
use crate::query::{
    binding::{analyze_query_plan_schema, bind_query_plan_schema},
    block::execute_query_block_output,
    cte::materialize_plan_ctes_with_filters,
    ordering::identity_order_columns,
    output::{QueryOutput, QueryRows},
    projection::{physical_exec_error, physical_work_mem_bytes},
    relational::{
        limit::attach_order_limit, row_count::resolve_limit_offset_with_ctes,
        values::execute_plan_values_output,
    },
    CteScope,
};
use std::{collections::BTreeSet, rc::Rc};
use uqa_sql::{
    ast::SetOpKind,
    plan::{AccessPathPlan, ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan},
    semantics::{
        cte_references_own_name, ordered_plan_ctes, reachable_plan_cte_names,
        sets::validation::validate_values_set_contexts, single_reference_plan_cte_names,
        volatility::query_contains_volatile_function,
    },
    SQLError, SQLParam, SQLResult,
};

pub fn execute_query_plan_with_ctes<S: Clone + Send + Sync + 'static>(
    context: &QueryContext<'_, S>,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
) -> Result<SQLResult, SQLError> {
    execute_query_plan_output(context, plan, params, ctes, QueryOutputMode::Rows)?.into_sql_result()
}

pub fn execute_query_plan_output<S: Clone + Send + Sync + 'static>(
    context: &QueryContext<'_, S>,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    output_mode: QueryOutputMode<S>,
) -> Result<QueryOutput, SQLError> {
    if plan.ctes.iter().any(|cte| cte.body.modifies_data()) {
        analyze_query_plan_schema(context.source.ctes.routines, plan, params, ctes, None)?;
        if ctes.command_cte_snapshot().is_none() {
            ctes.set_command_cte_snapshot(Some(std::sync::Arc::new(context.snapshots.capture()?)));
        }
    }
    let mut relation_lookup = ctes.enter_relation_lookup_mode(plan.relations_bound)?;
    let mut visible_ctes =
        relation_lookup.enter_visible_ctes(plan.ctes.iter().map(|cte| cte.name.as_str()));
    let ctes = &mut *visible_ctes;
    if !plan.ctes.is_empty() {
        let ordered_ctes = ordered_plan_ctes(plan)?;
        let reachable = reachable_plan_cte_names(plan);
        let single_reference = single_reference_plan_cte_names(plan);
        let recursive = ordered_ctes
            .iter()
            .copied()
            .filter(|cte| cte_references_own_name(cte))
            .map(|cte| cte.name.as_str())
            .collect::<BTreeSet<_>>();
        for cte in ordered_ctes.iter().copied().filter(|cte| {
            !cte.body.modifies_data()
                && !recursive.contains(cte.name.as_str())
                && reachable.contains(&cte.name)
                && match cte.materialization {
                    uqa_sql::ast::CteMaterialization::Default => {
                        single_reference.contains(&cte.name)
                    }
                    uqa_sql::ast::CteMaterialization::Materialized => false,
                    uqa_sql::ast::CteMaterialization::NotMaterialized => true,
                }
                && matches!(
                    cte.body
                        .query()
                        .map_or(Ok(true), |query| query_contains_volatile_function(
                            context.source.volatility,
                            query
                        )),
                    Ok(false)
                )
        }) {
            ctes.insert_deferred(cte.clone());
        }
        let filters = context.cte_filters.output_filters(plan, ctes)?;
        materialize_plan_ctes_with_filters(
            context.source.ctes,
            ordered_ctes.into_iter().filter(|cte| {
                reachable.contains(&cte.name)
                    && (cte.body.modifies_data()
                        || recursive.contains(cte.name.as_str())
                        || matches!(
                            cte.materialization,
                            uqa_sql::ast::CteMaterialization::Materialized
                        )
                        || (matches!(
                            cte.materialization,
                            uqa_sql::ast::CteMaterialization::Default
                        ) && !single_reference.contains(&cte.name))
                        || !matches!(
                            cte.body.query().map_or(Ok(true), |query| {
                                query_contains_volatile_function(context.source.volatility, query)
                            }),
                            Ok(false)
                        ))
            }),
            params,
            ctes,
            &filters,
        )?;
    }
    if let Some(snapshot) = ctes.command_cte_snapshot() {
        return with_query_snapshot(context.snapshots, &snapshot, |selected| {
            execute_query_root(selected, plan, params, ctes, output_mode)
        });
    }
    execute_query_root(context, plan, params, ctes, output_mode)
}
#[expect(
    clippy::too_many_lines,
    reason = "assembles set-operation execution and result delivery"
)]
fn execute_query_root<S: Clone + Send + Sync + 'static>(
    context: &QueryContext<'_, S>,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    output_mode: QueryOutputMode<S>,
) -> Result<QueryOutput, SQLError> {
    match &plan.root {
        RelationalPlan::QueryBlock(block) => execute_query_block_output(
            &context.source,
            block,
            params,
            ctes,
            bind_output_mode(context.generation, output_mode)?,
        ),
        RelationalPlan::SetOp {
            kind,
            all,
            left,
            right,
            order_by,
            limit,
            with_ties,
            offset,
            subqueries,
        } => {
            let set_schema =
                bind_query_plan_schema(context.source.ctes.routines, plan, params, ctes, None)?;
            let directional_union_all = matches!(
                &output_mode,
                QueryOutputMode::RowConsumer(downstream)
                    if downstream.uses_directional_scan()
                        && matches!((*kind, *all), (SetOpKind::Union, true))
                        && order_by.is_empty()
                        && !*with_ties
            );
            if directional_union_all {
                let left_schema =
                    bind_query_plan_schema(context.source.ctes.routines, left, params, ctes, None)?;
                let right_schema = bind_query_plan_schema(
                    context.source.ctes.routines,
                    right,
                    params,
                    ctes,
                    None,
                )?;
                let child_ctes = {
                    let child_scope = ctes.enter_lock_identity_emission(false);
                    (*child_scope).clone()
                };
                let left: Box<dyn crate::PhysicalOperator + '_> =
                    context.directional.query_operator(
                        (**left).clone(),
                        params.to_vec(),
                        child_ctes.clone(),
                        left_schema,
                    )?;
                let right: Box<dyn crate::PhysicalOperator + '_> = context
                    .directional
                    .query_operator((**right).clone(), params.to_vec(), child_ctes, right_schema)?;
                let mut operation: Box<dyn crate::PhysicalOperator + '_> = Box::new(
                    crate::ExternalSetOperation::new_directional_with_types(
                        left,
                        right,
                        *kind,
                        *all,
                        set_schema.column_types().to_vec(),
                        physical_work_mem_bytes(context.source.relational.runtime)?,
                    )
                    .map_err(physical_exec_error)?,
                );
                let (resolved_offset, resolved_limit) = {
                    let scoped_ctes = ctes.enter_scalar_subqueries(subqueries);
                    (
                        resolve_limit_offset_with_ctes(
                            offset.as_deref(),
                            context.source.relational,
                            params,
                            "OFFSET",
                            &scoped_ctes,
                        )?
                        .unwrap_or(0),
                        resolve_limit_offset_with_ctes(
                            limit.as_deref(),
                            context.source.relational,
                            params,
                            "LIMIT",
                            &scoped_ctes,
                        )?,
                    )
                };
                if resolved_offset != 0 || resolved_limit.is_some() {
                    operation = Box::new(crate::Limit::new(
                        operation,
                        resolved_offset,
                        resolved_limit,
                    ));
                }
                return collect_query_operator(
                    context,
                    set_schema.columns().to_vec(),
                    operation,
                    output_mode,
                );
            }
            let streaming_consumer = match &output_mode {
                QueryOutputMode::RowConsumer(downstream)
                    if matches!((*kind, *all), (SetOpKind::Union, true))
                        && order_by.is_empty()
                        && !*with_ties =>
                {
                    Some(Rc::clone(downstream))
                }
                _ => None,
            };
            if let Some(downstream) = streaming_consumer {
                let columns = set_schema.columns().to_vec();
                let column_types = set_schema.column_types().to_vec();
                let (resolved_offset, resolved_limit) = {
                    let scoped_ctes = ctes.enter_scalar_subqueries(subqueries);
                    (
                        resolve_limit_offset_with_ctes(
                            offset.as_deref(),
                            context.source.relational,
                            params,
                            "OFFSET",
                            &scoped_ctes,
                        )?
                        .unwrap_or(0),
                        resolve_limit_offset_with_ctes(
                            limit.as_deref(),
                            context.source.relational,
                            params,
                            "LIMIT",
                            &scoped_ctes,
                        )?,
                    )
                };
                let consumer = Rc::new(SetOperationConsumerFactory::new(
                    Rc::clone(&downstream),
                    set_schema.clone(),
                    resolved_offset,
                    resolved_limit,
                ));
                if consumer.stopped() {
                    Rc::clone(&downstream)
                        .bind(context.generation)?
                        .begin(&columns, &set_schema)?;
                } else {
                    let mut child_ctes = ctes.enter_lock_identity_emission(false);
                    execute_query_plan_output(
                        context,
                        left,
                        params,
                        &mut child_ctes,
                        QueryOutputMode::RowConsumer(consumer.clone()),
                    )?;
                    if !consumer.stopped() {
                        execute_query_plan_output(
                            context,
                            right,
                            params,
                            &mut child_ctes,
                            QueryOutputMode::RowConsumer(consumer),
                        )?;
                    }
                }
                return Ok(QueryOutput {
                    columns: columns.clone(),
                    column_types: column_types.clone(),
                    internal_columns: columns,
                    internal_types: column_types,
                    rows: QueryRows::Rows {
                        named: Vec::new(),
                        positional: None,
                    },
                });
            }
            // Materialize each child directly into a disk-backed, repeatable stream before starting the next child. A nested set operation therefore never owns two cardinality-sized `SQLResult.rows` vectors, and its external merge consumes batches under `work_mem`.
            let (lhs, rhs) = {
                let mut child_ctes = ctes.enter_lock_identity_emission(false);
                let lhs = execute_query_plan_output(
                    context,
                    left,
                    params,
                    &mut child_ctes,
                    QueryOutputMode::SharedSpill,
                )?;
                let rhs = execute_query_plan_output(
                    context,
                    right,
                    params,
                    &mut child_ctes,
                    QueryOutputMode::SharedSpill,
                )?;
                (lhs, rhs)
            };
            let columns = lhs.columns.clone();
            let left: Box<dyn crate::PhysicalOperator + '_> = lhs.into_public_operator();
            let right: Box<dyn crate::PhysicalOperator + '_> = rhs.into_public_operator();
            let operation: Box<dyn crate::PhysicalOperator + '_> = Box::new(
                crate::ExternalSetOperation::new_with_types(
                    left,
                    right,
                    *kind,
                    *all,
                    set_schema.column_types().to_vec(),
                    physical_work_mem_bytes(context.source.relational.runtime)?,
                )
                .map_err(physical_exec_error)?,
            );
            if !order_by.is_empty() || limit.is_some() || offset.is_some() {
                let synthetic = QueryBlockPlan {
                    projections: Vec::new(),
                    from: None,
                    r#where: None,
                    compute: ComputePlan::Project,
                    group_by: Vec::new(),
                    grouping_sets: Vec::new(),
                    group_distinct: false,
                    having: None,
                    order_by: order_by.clone(),
                    limit: limit.as_deref().cloned(),
                    with_ties: *with_ties,
                    offset: offset.as_deref().cloned(),
                    distinct: false,
                    distinct_on: Vec::new(),
                    subqueries: subqueries.clone(),
                    access: AccessPathPlan::Row,
                    locking: Vec::new(),
                };
                let ordering_scope = ctes.enter_scalar_subqueries(subqueries);
                let evaluator = context.source.relational.evaluator(params, &ordering_scope);
                let output = identity_order_columns(&columns);
                let operation = attach_order_limit(
                    operation,
                    &synthetic,
                    &output,
                    context.source.relational,
                    params,
                    &ordering_scope,
                    context.source.relational.runtime,
                    evaluator,
                    None,
                )?;
                return collect_query_operator(context, columns, operation, output_mode);
            }
            collect_query_operator(context, columns, operation, output_mode)
        }
        RelationalPlan::Values { rows, subqueries } => {
            {
                let scoped_ctes = ctes.enter_scalar_subqueries(subqueries);
                let type_resolver = context
                    .source
                    .relational
                    .expression_scope((*scoped_ctes).clone());
                validate_values_set_contexts(
                    context.source.relational.catalog,
                    type_resolver.as_ref(),
                    rows,
                    &crate::RowSchema::default(),
                    params,
                )?;
            }
            execute_plan_values_output(
                context.source.relational,
                rows,
                subqueries,
                params,
                ctes,
                bind_output_mode(context.generation, output_mode)?,
            )
        }
    }
}

pub fn collect_query_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &QueryContext<'_, S>,
    columns: Vec<String>,
    operator: Box<dyn crate::PhysicalOperator + 'a>,
    output_mode: QueryOutputMode<S>,
) -> Result<QueryOutput, SQLError> {
    crate::query::collection::collect_query_operator(
        context.source.relational.runtime,
        columns,
        operator,
        bind_output_mode(context.generation, output_mode)?,
    )
}
