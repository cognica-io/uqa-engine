//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared CTE dependencies, reachability and stable ordering under the invoking allowance.

pub(super) mod analysis;
mod dependencies;
pub(super) mod references;
#[cfg(test)]
mod tests;

use crate::{
    plan::{CommandPlan, CtePlan, PlanRef, QueryPlan},
    SQLError,
};
use analysis::CteAnalysis;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

/// Reachable declaration indices in the native dependency order. Unread query definitions are omitted; data-modifying definitions are always included.
pub fn reachable_query_cte_order_with_control(
    query: &QueryPlan,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<usize>>, SQLError> {
    reachable_order(&CteAnalysis::query(query, control)?, control)
}

/// Apply the same reachability and dependency rules to a command's WITH list.
pub fn reachable_command_cte_order_with_control(
    command: &CommandPlan,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<usize>>, SQLError> {
    reachable_order(&CteAnalysis::command(command, control)?, control)
}

/// Select the definitions reachable from a statement. Recursive dependency cycles are rejected even among unread definitions.
pub fn reachable_cte_declarations_with_control(
    statement: PlanRef<'_>,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<bool>>, SQLError> {
    declaration_analysis(statement, control)?.reachable()
}

fn declaration_analysis<'p, 'c>(
    statement: PlanRef<'p>,
    control: ProductionControl<'c>,
) -> Result<CteAnalysis<'p, 'c>, SQLError> {
    let analysis = match statement {
        PlanRef::Query(query) => CteAnalysis::query(query, control)?,
        PlanRef::Command(command) => CteAnalysis::command(command, control)?,
    };
    if analysis.plans.iter().any(|plan| plan.recursive) {
        drop(analysis.order()?);
    }
    Ok(analysis)
}

/// `PostgreSQL` plans referenced materialized definitions before pruning consumers. Their constants undergo planning even when execution never requests a row.
pub fn planned_cte_declarations_with_control(
    statement: PlanRef<'_>,
    catalog: Option<&dyn super::volatility::VolatilityCatalog>,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<bool>>, SQLError> {
    let catalog = catalog.unwrap_or(&super::volatility::BuiltinVolatilityCatalog);
    let analysis = declaration_analysis(statement, control)?;
    let live = analysis.reachable()?;
    let mut additional = ProductionVec::new(control);
    for (index, &read) in live.iter().enumerate() {
        control.check()?;
        if !read
            && analysis.has_reference(index)
            && !super::cte_strategy::can_defer(&analysis, index, catalog, control)?
        {
            additional.push_copy(index)?;
        }
    }
    if additional.is_empty() {
        return Ok(live);
    }
    analysis.closure(
        live.iter()
            .enumerate()
            .filter_map(|(index, &read)| read.then_some(index))
            .chain(additional.iter().copied()),
    )
}

fn reachable_order(
    analysis: &CteAnalysis<'_, '_>,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<usize>>, SQLError> {
    let ordered = analysis.order()?;
    let reachable = analysis.reachable()?;
    let mut output = ProductionVec::new(control);
    for index in ordered.iter().copied() {
        control.check()?;
        if reachable[index] {
            output.push_copy(index)?;
        }
    }
    Ok(output.finish()?)
}

/// Order selected definitions without copying their names or bodies. The result retains its reference-array allowance until its consumer finishes.
pub fn order_cte_plans_with_control<'p>(
    plans: impl IntoIterator<Item = &'p CtePlan>,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<&'p CtePlan>>, SQLError> {
    let mut selected = ProductionVec::new(control);
    for plan in plans {
        selected.push_copy(plan)?;
    }
    if !selected.iter().any(|plan| plan.recursive) {
        return Ok(selected.finish()?);
    }
    let analysis = CteAnalysis::new(selected.iter().copied(), control)?;
    let ordered = analysis.order()?;
    for (destination, index) in selected.as_mut_slice().iter_mut().zip(ordered.iter()) {
        control.check()?;
        *destination = analysis.plans[*index];
    }
    Ok(selected.finish()?)
}

/// A caller editing the original CTEs needs declaration positions, in the same dependency order used for borrowed plans.
pub(crate) async fn cte_positions_with_work<W: crate::catalog::work::Work>(
    plans: &[CtePlan],
    control: ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<Vec<usize>>, SQLError> {
    let mut declared = ProductionVec::new(control);
    for (position, plan) in plans.iter().enumerate() {
        work.checkpoint().await?;
        if plan.recursive {
            drop(declared);
            return CteAnalysis::new(plans, control)?.order();
        }
        declared.push_copy(position)?;
    }
    Ok(declared.finish()?)
}
