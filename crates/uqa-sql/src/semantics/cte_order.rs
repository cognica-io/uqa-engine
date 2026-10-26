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
    plan::{CommandPlan, CtePlan, QueryPlan},
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
