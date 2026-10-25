//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! When the WITH items of a statement whose WITH modifies data run, as `PostgreSQL`'s executor runs them.

use super::{install_scheduled_ctes, CteExecutionContext};
use crate::query::CteScope;
use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::{
    plan::CtePlan,
    semantics::{
        cte_strategy::{order_statement_ctes, ScheduledCte},
        order_cte_plans,
    },
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
    install_statement_ctes(
        context,
        ctes,
        scheduled
            .into_iter()
            .map(|plan| ScheduledCte {
                plan,
                deferred: false,
            })
            .collect(),
        primary_references,
        params,
        scope,
        output_filters,
    )
}

pub(crate) fn install_statement_ctes<'a, S: Clone>(
    context: CteExecutionContext<'_, S>,
    ctes: &'a [CtePlan],
    scheduled: Vec<ScheduledCte<'a>>,
    primary_references: impl FnOnce() -> BTreeSet<String>,
    params: &[SQLParam],
    scope: &mut CteScope<S>,
    output_filters: &BTreeMap<String, (String, ScalarExpr)>,
) -> Result<(), SQLError> {
    if !scheduled.iter().any(|cte| cte.plan.body.modifies_data()) {
        return install_scheduled_ctes(context, scheduled, params, scope, output_filters);
    }
    let folded = scheduled
        .iter()
        .filter(|cte| cte.deferred)
        .map(|cte| cte.plan.name.as_str())
        .collect::<BTreeSet<_>>();
    let order = order_statement_ctes(
        ctes,
        order_cte_plans(scheduled.iter().map(|cte| cte.plan).collect())?,
        &primary_references(),
    );
    let commands = match scope.statement_commands() {
        Some(commands) => std::sync::Arc::clone(commands),
        None => {
            let commands = std::sync::Arc::default();
            scope.set_statement_commands(std::sync::Arc::clone(&commands));
            commands
        }
    };
    commands.begin_data_modifying_with(
        order
            .postponed
            .into_iter()
            .map(|plan| (plan.clone(), folded.contains(plan.name.as_str())))
            .collect(),
    );
    install_scheduled_ctes(
        context,
        order.primary.into_iter().map(|plan| ScheduledCte {
            deferred: folded.contains(plan.name.as_str()),
            plan,
        }),
        params,
        scope,
        output_filters,
    )
}

/// Materialize the WITH items of a data-modifying statement as [`materialize_statement_ctes`] does; `primary_references` names the items the statement's own clauses read.
pub fn materialize_command_ctes<S: Clone>(
    context: CteExecutionContext<'_, S>,
    ctes: &[CtePlan],
    primary_references: impl FnOnce() -> BTreeSet<String>,
    params: &[SQLParam],
    scope: &mut CteScope<S>,
) -> Result<(), SQLError> {
    let primary_references = primary_references();
    let reachable =
        uqa_sql::semantics::reachable_statement_cte_names(ctes, primary_references.clone());
    materialize_statement_ctes(
        context,
        ctes,
        ctes.iter()
            .filter(|cte| reachable.contains(&cte.name))
            .collect(),
        || primary_references,
        params,
        scope,
        &BTreeMap::new(),
    )
}

/// Run the items a statement kept for after its primary query, in the order [`materialize_statement_ctes`] kept them.
pub fn finish_statement_ctes<S: Clone>(
    context: CteExecutionContext<'_, S>,
    params: &[SQLParam],
    scope: &mut CteScope<S>,
) -> Result<(), SQLError> {
    let Some(commands) = scope.statement_commands().cloned() else {
        return Ok(());
    };
    let postponed = commands.take_scheduled_postponed();
    install_scheduled_ctes(
        context,
        postponed.iter().map(|(plan, deferred)| ScheduledCte {
            plan,
            deferred: *deferred,
        }),
        params,
        scope,
        &BTreeMap::new(),
    )
}
