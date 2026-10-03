//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! When the WITH items of a statement whose WITH modifies data run, as `PostgreSQL`'s executor runs them.

use super::{materialize_plan_ctes, materialize_plan_ctes_with_filters, CteExecutionContext};
use crate::query::scope::AfterEventFiring;
use crate::query::CteScope;
use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::{
    plan::CtePlan,
    semantics::{cte_strategy::order_statement_ctes, order_cte_plans},
    SQLError, SQLParam, ScalarExpr,
};

/// Materialize `scheduled`, the items of `ctes` that a statement materializes, when the statement's WITH modifies data: the items its primary query reads run now, and the scope keeps the others for [`finish_statement_ctes`], which runs them once the primary query has finished, as `ExecPostprocessPlan` does. `primary_references` names the items the primary query reads. A statement whose WITH only reads materializes every item now.
pub fn materialize_statement_ctes<'a, S: Clone>(
    context: CteExecutionContext<'_, S>,
    ctes: &'a [CtePlan],
    scheduled: Vec<&'a CtePlan>,
    primary_references: impl FnOnce() -> BTreeSet<String>,
    params: &[SQLParam],
    scope: &mut CteScope<S>,
    output_filters: &BTreeMap<String, (String, ScalarExpr)>,
) -> Result<(), SQLError> {
    if !scheduled.iter().any(|cte| cte.body.modifies_data()) {
        return materialize_plan_ctes_with_filters(
            context,
            scheduled,
            params,
            scope,
            output_filters,
        );
    }
    let order = order_statement_ctes(ctes, order_cte_plans(scheduled)?, &primary_references());
    scope.begin_statement_commands(order.postponed.into_iter().cloned().collect());
    materialize_plan_ctes_with_filters(context, order.primary, params, scope, output_filters)
}

/// Materialize the WITH items of a data-modifying statement as [`materialize_statement_ctes`] does; `primary_references` names the items the statement's own clauses read.
pub fn materialize_command_ctes<S: Clone>(
    context: CteExecutionContext<'_, S>,
    ctes: &[CtePlan],
    primary_references: impl FnOnce() -> BTreeSet<String>,
    params: &[SQLParam],
    scope: &mut CteScope<S>,
) -> Result<(), SQLError> {
    materialize_statement_ctes(
        context,
        ctes,
        ctes.iter().collect(),
        primary_references,
        params,
        scope,
        &BTreeMap::new(),
    )
}

/// Run the items a statement kept for after its primary query, in the order [`materialize_statement_ctes`] kept them, and hand back every AFTER event the statement's commands queued, in the order they fire.
pub fn finish_statement_ctes<S: Clone>(
    context: CteExecutionContext<'_, S>,
    params: &[SQLParam],
    scope: &mut CteScope<S>,
) -> Result<Vec<AfterEventFiring>, SQLError> {
    let Some(commands) = scope.statement_commands().cloned() else {
        return Ok(Vec::new());
    };
    let postponed = commands.take_postponed();
    materialize_plan_ctes(context, &postponed, params, scope)?;
    Ok(commands.take_after_events())
}
