//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schedule mandatory WITH writes under the invoking query's retained allowance.

use super::{install_scheduled_ctes, CteExecutionContext};
use crate::query::CteScope;
use std::collections::BTreeMap;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationName,
};
use uqa_sql::{
    plan::CtePlan,
    semantics::{
        cte_strategy::{order_statement_ctes_with_control, ScheduledCte},
        order_cte_plans_with_control,
    },
    SQLError, SQLParam, ScalarExpr,
};

/// Materialize the selected WITH items in their native before/after-primary order. The root-reference producer borrows definition names under the same allowance.
pub fn materialize_statement_ctes<'a, S: Clone>(
    context: CteExecutionContext<'_, S>,
    ctes: &'a [CtePlan],
    scheduled: impl IntoIterator<Item = &'a CtePlan>,
    primary_references: impl FnOnce(ProductionControl<'_>) -> Result<Produced<Vec<&'a str>>, SQLError>,
    params: &[SQLParam],
    scope: &mut CteScope<S>,
    output_filters: &BTreeMap<String, (String, ScalarExpr)>,
) -> Result<(), SQLError> {
    let retained = context.queries.read_control()?;
    let control = ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.runtime.cancellation,
    );
    let mut selected = ProductionVec::new(control);
    for plan in scheduled {
        selected.push_copy(ScheduledCte {
            plan,
            deferred: false,
        })?;
    }
    install_statement_ctes(
        context,
        ctes,
        &selected,
        primary_references,
        params,
        scope,
        output_filters,
    )
}

pub(crate) fn install_statement_ctes<'a, S: Clone>(
    context: CteExecutionContext<'_, S>,
    ctes: &'a [CtePlan],
    scheduled: &[ScheduledCte<'a>],
    primary_references: impl FnOnce(ProductionControl<'_>) -> Result<Produced<Vec<&'a str>>, SQLError>,
    params: &[SQLParam],
    scope: &mut CteScope<S>,
    output_filters: &BTreeMap<String, (String, ScalarExpr)>,
) -> Result<(), SQLError> {
    if !scheduled.iter().any(|cte| cte.plan.body.modifies_data()) {
        return install_scheduled_ctes(
            context,
            scheduled.iter().copied(),
            params,
            scope,
            output_filters,
        );
    }
    let retained = context.queries.read_control()?;
    let control = ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.runtime.cancellation,
    );
    let mut folded = ProductionVec::new(control);
    for cte in scheduled.iter().filter(|cte| cte.deferred) {
        folded.push_copy(RelationName::decoded(&cte.plan.name))?;
    }
    folded.as_mut_slice().sort_unstable();
    let ordered = order_cte_plans_with_control(scheduled.iter().map(|cte| cte.plan), control)?;
    let primary = primary_references(control)?;
    let order = order_statement_ctes_with_control(
        ctes,
        ordered.iter().copied(),
        primary.iter().copied(),
        control,
    )?;
    let commands = match scope.statement_commands() {
        Some(commands) => std::sync::Arc::clone(commands),
        None => crate::query::scope::StatementCommands::with_control(&control)?,
    };
    let is_folded = |plan: &CtePlan| {
        folded
            .binary_search(&RelationName::decoded(&plan.name))
            .is_ok()
    };
    commands.begin_data_modifying_with(
        order.postponed.iter().map(|plan| ScheduledCte {
            plan,
            deferred: is_folded(plan),
        }),
        &control,
    )?;
    scope.set_statement_commands(commands);
    install_scheduled_ctes(
        context,
        order.primary.iter().map(|plan| ScheduledCte {
            deferred: is_folded(plan),
            plan,
        }),
        params,
        scope,
        output_filters,
    )
}

/// Materialize the reachable definitions of a mutation, including every mandatory writer.
pub fn materialize_command_ctes<'a, S: Clone>(
    context: CteExecutionContext<'_, S>,
    ctes: &'a [CtePlan],
    primary_references: impl FnOnce(ProductionControl<'_>) -> Result<Produced<Vec<&'a str>>, SQLError>,
    params: &[SQLParam],
    scope: &mut CteScope<S>,
) -> Result<(), SQLError> {
    let retained = context.queries.read_control()?;
    let control = ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.runtime.cancellation,
    );
    let primary = primary_references(control)?;
    let reachable = uqa_sql::semantics::reachable_statement_ctes_with_control(
        ctes,
        primary.iter().copied(),
        control,
    )?;
    materialize_statement_ctes(
        context,
        ctes,
        reachable.iter().copied(),
        |_| Ok(primary),
        params,
        scope,
        &BTreeMap::new(),
    )
}

/// Run the definitions retained for after the primary query, in their native dependency order.
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
        postponed
            .iter()
            .flat_map(|schedule| schedule.iter())
            .map(|(plan, deferred)| ScheduledCte {
                plan,
                deferred: *deferred,
            }),
        params,
        scope,
        &BTreeMap::new(),
    )
}
