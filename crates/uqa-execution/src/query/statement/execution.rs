//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTE scheduling, statement snapshot selection, and physical query execution.
mod materialized;

use super::{
    bound_consumer::bind_output_mode,
    consumer::{QueryOutputMode, SetOperationConsumerFactory},
    context::{with_query_snapshot, QueryContext},
};
use crate::query::{
    binding::{analyze_query_plan_schema, bind_query_plan_schema},
    block::execute_query_block_output,
    cte::{finish_statement_ctes, install_statement_ctes, strategy::schedule_plan_ctes},
    ordering::identity_order_columns,
    output::{QueryOutput, QueryRows},
    projection::{physical_exec_error, physical_work_mem_bytes},
    relational::{
        limit::attach_order_limit, row_count::resolve_limit_offset_with_ctes,
        values::execute_plan_values_output,
    },
    CteScope,
};
use std::rc::Rc;
use uqa_sql::{
    ast::SetOpKind,
    plan::{AccessPathPlan, ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan},
    semantics::{primary_query_cte_references, sets::validation::validate_values_set_contexts},
    SQLError, SQLParam, SQLResult,
};

pub fn execute_query_plan_with_ctes<S: Clone + Send + Sync + 'static>(
    context: &QueryContext<'_, S>,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
) -> Result<SQLResult, SQLError> {
    materialized::collect(|mode| execute_query_plan_output(context, plan, params, ctes, mode))
}

pub fn execute_query_plan_output<S: Clone + Send + Sync + 'static>(
    context: &QueryContext<'_, S>,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    output_mode: QueryOutputMode<S>,
) -> Result<QueryOutput, SQLError> {
    let _diagnostics = context
        .source
        .relational
        .runtime
        .diagnostics
        .bind_current()
        .map_err(|error| crate::storage_errors::storage_error("bind query diagnostics", &error))?;
    let prepared =
        crate::query::sources::prepared_queries::prepare(&context.source, plan, params, ctes)?;
    let plan = prepared.as_ref();
    crate::query::privileges::routine_calls::query(&context.source, plan, params, ctes)?;
    let mut running_statement = None;
    if plan.ctes.iter().any(|cte| cte.body.modifies_data()) {
        analyze_query_plan_schema(context.source.ctes.routines, plan, params, ctes, None)?;
        if ctes.command_cte_snapshot().is_none() {
            ctes.set_command_cte_snapshot(Some(std::sync::Arc::new(context.snapshots.capture()?)));
        }
        if ctes.statement_commands().is_none() {
            let statement = std::sync::Arc::<crate::query::scope::StatementCommands>::default();
            ctes.set_statement_commands(std::sync::Arc::clone(&statement));
            running_statement = Some(crate::mutation::statement_end::enter_statement(statement));
        }
    }
    let result = ctes.with_cte_producers(!plan.ctes.is_empty(), |ctes| {
        execute_prepared_query_output(context, plan, params, ctes, output_mode)
    });
    drop(running_statement);
    result
}

fn execute_prepared_query_output<S: Clone + Send + Sync + 'static>(
    context: &QueryContext<'_, S>,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    output_mode: QueryOutputMode<S>,
) -> Result<QueryOutput, SQLError> {
    let mut relation_lookup = ctes.enter_relation_lookup_mode(plan.relations_bound)?;
    let mut visible_ctes =
        relation_lookup.enter_visible_ctes(plan.ctes.iter().map(|cte| cte.name.as_str()));
    let ctes = &mut *visible_ctes;
    install_query_ctes(context, plan, params, ctes)?;
    let output = match ctes.command_cte_snapshot() {
        Some(snapshot) => with_query_snapshot(context.snapshots, &snapshot, |selected| {
            execute_query_root(selected, plan, params, ctes, output_mode)
        })?,
        None => execute_query_root(context, plan, params, ctes, output_mode)?,
    };
    if plan.ctes.iter().any(|cte| cte.body.modifies_data()) {
        finish_statement_ctes(context.source.ctes, params, ctes)?;
        if let Some(commands) = ctes.statement_commands().cloned() {
            context
                .source
                .ctes
                .queries
                .fire_after_triggers(commands.after_triggers())?;
        }
    }
    Ok(output)
}

fn install_query_ctes<S: Clone + Send + Sync + 'static>(
    context: &QueryContext<'_, S>,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
) -> Result<(), SQLError> {
    if plan.ctes.is_empty() {
        return Ok(());
    }
    let scheduled = schedule_plan_ctes(context.source.volatility, plan)?;
    let filters = context.cte_filters.output_filters(plan, ctes)?;
    install_statement_ctes(
        context.source.ctes,
        &plan.ctes,
        scheduled,
        || primary_query_cte_references(plan),
        params,
        ctes,
        &filters,
    )
}
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
            bind_output_mode(
                context.generation,
                output_mode,
                context.source.types.enum_labels(),
            )?,
        ),
        RelationalPlan::SetOp { .. } => {
            execute_set_operation(context, plan, params, ctes, output_mode)
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
                bind_output_mode(
                    context.generation,
                    output_mode,
                    context.source.types.enum_labels(),
                )?,
            )
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "assembles set-operation execution and result delivery"
)]
fn execute_set_operation<S: Clone + Send + Sync + 'static>(
    context: &QueryContext<'_, S>,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    output_mode: QueryOutputMode<S>,
) -> Result<QueryOutput, SQLError> {
    let RelationalPlan::SetOp {
        kind,
        all,
        left,
        right,
        order_by,
        limit,
        with_ties,
        offset,
        subqueries,
    } = &plan.root
    else {
        unreachable!("set-operation dispatch requires a set-operation root");
    };
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
        let right_schema =
            bind_query_plan_schema(context.source.ctes.routines, right, params, ctes, None)?;
        let child_ctes = {
            let child_scope = ctes.enter_lock_identity_emission(false);
            (*child_scope).clone()
        };
        let left: Box<dyn crate::PhysicalOperator + '_> = context.directional.query_operator(
            (**left).clone(),
            params.to_vec(),
            child_ctes.clone(),
            left_schema,
        )?;
        let (left, right) = crate::physical::construction::OperatorBuild::run(left, |_| {
            context.directional.query_operator(
                (**right).clone(),
                params.to_vec(),
                child_ctes,
                right_schema,
            )
        })?;
        let (left, right, work_mem) =
            crate::physical::construction::OperatorBuild::pair(left, right, |_, _| {
                physical_work_mem_bytes(context.source.relational.runtime)
            })?;
        let mut operation: Box<dyn crate::PhysicalOperator + '_> = Box::new(
            crate::ExternalSetOperation::new_directional_with_types(
                left,
                right,
                *kind,
                *all,
                set_schema.column_types().to_vec(),
                work_mem,
            )
            .map_err(physical_exec_error)?,
        );
        let (input, (resolved_offset, resolved_limit)) =
            crate::physical::construction::OperatorBuild::run(operation, |_| {
                let scoped_ctes = ctes.enter_scalar_subqueries(subqueries);
                Ok((
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
                ))
            })?;
        operation = input;
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
            if downstream.streams_set_inputs()
                && matches!((*kind, *all), (SetOpKind::Union, true))
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
            columns,
            column_types,
            internal_schema: set_schema,
            rows: QueryRows::Rows(Vec::new()),
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
        crate::ExternalSetOperation::new_with_types_and_evaluator(
            left,
            right,
            *kind,
            *all,
            set_schema.column_types().to_vec(),
            physical_work_mem_bytes(context.source.relational.runtime)?,
            context.source.relational.evaluator(params, ctes),
        )
        .map_err(physical_exec_error)?,
    );
    if !order_by.is_empty() || limit.is_some() || offset.is_some() {
        let synthetic = QueryBlockPlan {
            deferred_outer_rewrites: Vec::new(),
            privilege_columns: std::collections::BTreeSet::default(),
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
            windows: Vec::new(),
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

pub fn collect_query_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &QueryContext<'_, S>,
    columns: Vec<String>,
    operator: Box<dyn crate::PhysicalOperator + 'a>,
    output_mode: QueryOutputMode<S>,
) -> Result<QueryOutput, SQLError> {
    let (operator, output_mode) =
        crate::physical::construction::OperatorBuild::run(operator, |_| {
            bind_output_mode(
                context.generation,
                output_mode,
                context.source.types.enum_labels(),
            )
        })?;
    crate::query::collection::collect_query_operator(
        context.source.relational.runtime,
        columns,
        operator,
        output_mode,
    )
}
