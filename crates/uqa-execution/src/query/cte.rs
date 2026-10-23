//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTE execution and recursive working-table materialization.

use super::{
    collection::collect_query_operator,
    consumer::QueryOutputMode,
    output::{QueryOutput, QueryRows},
    CteScope,
};
use std::collections::BTreeMap;
use uqa_sql::{
    plan::{CtePlan, CtePlanBody},
    semantics::{cte_references_own_name, order_cte_plans},
    SQLError, SQLParam, ScalarExpr,
};
pub mod context;
pub mod recursive;
pub mod rows;
mod statement;
pub(crate) mod strategy;
pub use context::CteExecutionContext;
use recursive::materialize_recursive_cte;
pub(crate) use statement::install_statement_ctes;
pub use statement::{finish_statement_ctes, materialize_command_ctes, materialize_statement_ctes};

/// Bind folded definitions only after their dependencies are available, retaining the lexical input of each definition before later names replace it.
pub(crate) fn install_scheduled_ctes<'a, S: Clone>(
    context: CteExecutionContext<'_, S>,
    plans: impl IntoIterator<Item = uqa_sql::semantics::cte_strategy::ScheduledCte<'a>>,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    output_filters: &BTreeMap<String, (String, ScalarExpr)>,
) -> Result<(), SQLError> {
    for scheduled in plans {
        if scheduled.deferred {
            ctes.insert_deferred(scheduled.plan.clone());
        } else {
            materialize_plan_ctes_with_filters(
                context,
                std::iter::once(scheduled.plan),
                params,
                ctes,
                output_filters,
            )?;
        }
    }
    Ok(())
}

pub fn materialize_plan_ctes<S: Clone>(
    context: CteExecutionContext<'_, S>,
    plans: &[CtePlan],
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
) -> Result<(), SQLError> {
    materialize_plan_ctes_with_filters(context, plans, params, ctes, &BTreeMap::new())
}

pub fn materialize_plan_ctes_with_filters<'a, S: Clone>(
    context: CteExecutionContext<'_, S>,
    plans: impl IntoIterator<Item = &'a CtePlan>,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
    output_filters: &BTreeMap<String, (String, ScalarExpr)>,
) -> Result<(), SQLError> {
    let plans = order_cte_plans(plans.into_iter().collect())?;
    for plan in plans {
        if cte_references_own_name(plan) {
            let rows = {
                let mut cte_scope = ctes.enter_lock_identity_emission(false);
                materialize_recursive_cte(
                    context,
                    plan,
                    params,
                    &mut cte_scope,
                    output_filters.get(&plan.name),
                )?
            };
            ctes.insert_shared(plan.name.clone(), rows);
            continue;
        }

        if install_read_only_prefix(context, plan, params, ctes)? {
            continue;
        }
        let outer_row = ctes.row_lock_outer_row().cloned();
        let result = {
            let mut cte_scope = ctes.enter_lock_identity_emission(false);
            match &plan.body {
                CtePlanBody::Query(query) => {
                    if let Some(outer_row) = outer_row.as_ref() {
                        context
                            .queries
                            .execute_lateral_query(query, outer_row, params, &cte_scope)?
                    } else {
                        context
                            .queries
                            .execute_query(query, params, &mut cte_scope)?
                    }
                }
                CtePlanBody::Command(command) => {
                    execute_command_cte(context, command, params, &cte_scope)?
                }
            }
        };
        let mut columns = result.columns.clone();
        let source_columns = result.internal_schema.columns().to_vec();
        let mut operator = result.into_operator();
        if !plan.columns.is_empty() {
            let renamed_columns = columns
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    plan.columns
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| source.clone())
                })
                .collect::<Vec<_>>();
            let mapping = source_columns
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    let output = renamed_columns
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| source.clone());
                    (output, index)
                })
                .collect();
            columns = renamed_columns;
            operator = Box::new(crate::ColumnSelection::with_positions(operator, mapping));
        }
        let identity = operator
            .row_schema()
            .columns()
            .iter()
            .cloned()
            .enumerate()
            .map(|(position, column)| (column, position))
            .collect();
        operator = Box::new(
            crate::ColumnSelection::with_positions(operator, identity).discarding_lock_origins(),
        );
        let materialized = collect_query_operator(
            context.runtime,
            columns,
            operator,
            QueryOutputMode::SharedSpill,
        )?;
        let QueryRows::SharedSpill(materialized) = materialized.rows else {
            return Err(SQLError::Internal(
                "CTE spill collector returned in-memory rows".into(),
            ));
        };
        ctes.insert_shared(plan.name.clone(), materialized);
        if !plan.body.returns_rows() {
            ctes.non_returning_ctes.insert(plan.name.clone());
        }
    }
    Ok(())
}

fn install_read_only_prefix<S: Clone>(
    context: CteExecutionContext<'_, S>,
    plan: &CtePlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
) -> Result<bool, SQLError> {
    if let CtePlanBody::Query(query) = &plan.body {
        let scoped = {
            let scope = ctes.enter_lock_identity_emission(false);
            (*scope).clone()
        };
        if let Some(operator) = context.queries.retained_query(query, params, &scoped)? {
            let columns = operator
                .schema()
                .iter()
                .enumerate()
                .map(|(position, column)| {
                    (
                        plan.columns
                            .get(position)
                            .cloned()
                            .unwrap_or_else(|| column.clone()),
                        position,
                    )
                })
                .collect();
            let operator = Box::new(
                crate::ColumnSelection::with_positions(operator, columns).discarding_lock_origins(),
            );
            ctes.insert_rows(
                plan.name.clone(),
                rows::CteRows::from_producer(operator, context.runtime.work_mem_bytes()?),
            );
            return Ok(true);
        }
    }
    Ok(false)
}

fn execute_command_cte<S: Clone>(
    context: CteExecutionContext<'_, S>,
    command: &uqa_sql::plan::CommandPlan,
    params: &[SQLParam],
    scope: &CteScope<S>,
) -> Result<QueryOutput, SQLError> {
    let result = context.queries.execute_command(command, params, scope)?;
    let columns = result.schema.columns().to_vec();
    collect_query_operator(
        context.runtime,
        columns,
        result.into_operator(),
        QueryOutputMode::SharedSpill,
    )
}
