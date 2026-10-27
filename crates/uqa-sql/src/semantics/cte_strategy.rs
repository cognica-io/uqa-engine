//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared CTE scheduling decisions for execution and traversal analysis.

use crate::{
    ast::CteMaterialization,
    plan::{CtePlan, QueryPlan},
    semantics::{
        cte_order::analysis::CteAnalysis,
        volatility::{query_contains_volatile_function, VolatilityCatalog},
    },
    SQLError,
};
use std::collections::BTreeSet;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

#[derive(Clone, Copy, Debug)]
pub struct ScheduledCte<'a> {
    pub plan: &'a CtePlan,
    pub deferred: bool,
}

pub fn schedule_plan_ctes<'a>(
    catalog: &dyn VolatilityCatalog,
    plan: &'a QueryPlan,
) -> Result<Vec<ScheduledCte<'a>>, SQLError> {
    Ok(
        schedule_plan_ctes_with_control(catalog, plan, ProductionControl::uncontrolled())?
            .into_uncontrolled()
            .expect("ordinary CTE schedule"),
    )
}

pub fn schedule_plan_ctes_with_control<'a>(
    catalog: &dyn VolatilityCatalog,
    plan: &'a QueryPlan,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<ScheduledCte<'a>>>, SQLError> {
    let analysis = CteAnalysis::query(plan, control)?;
    let ordered = analysis.order()?;
    let reachable = analysis.reachable()?;
    let mut scheduled = ProductionVec::new(control);
    for index in ordered.iter().copied() {
        control.check()?;
        if !reachable[index] {
            continue;
        }
        let cte = &plan.ctes[index];
        let deferred = !cte.body.modifies_data()
            && !analysis.recursive(index)
            && match cte.materialization {
                CteMaterialization::Default => analysis.single_reference(index),
                CteMaterialization::Materialized => false,
                CteMaterialization::NotMaterialized => true,
            }
            && matches!(
                cte.body
                    .query()
                    .map_or(Ok(true), |query| query_contains_volatile_function(
                        catalog, query
                    )),
                Ok(false)
            );
        scheduled.push_copy(ScheduledCte {
            plan: cte,
            deferred,
        })?;
    }
    Ok(scheduled.finish()?)
}

/// Materialized WITH items split by native execution time: read dependencies precede the primary query; unread mutations finish afterward, last defined first.
#[derive(Debug)]
pub struct StatementCteOrder<'a> {
    pub primary: Vec<&'a CtePlan>,
    pub postponed: Vec<&'a CtePlan>,
}

pub fn order_statement_ctes<'a>(
    ctes: &'a [CtePlan],
    scheduled: Vec<&'a CtePlan>,
    primary_references: &BTreeSet<String>,
) -> StatementCteOrder<'a> {
    order_statement_ctes_with_control(
        ctes,
        scheduled,
        primary_references.iter().map(String::as_str),
        ProductionControl::uncontrolled(),
    )
    .expect("ordinary statement CTE order")
    .into_uncontrolled()
    .expect("ordinary statement CTE owner")
}

/// Allocate dependency and ordering scratch under the invoking owner, retaining both returned arrays with that allowance.
pub fn order_statement_ctes_with_control<'a, 'n>(
    ctes: &'a [CtePlan],
    scheduled: impl IntoIterator<Item = &'a CtePlan>,
    primary_references: impl IntoIterator<Item = &'n str>,
    control: ProductionControl<'_>,
) -> Result<Produced<StatementCteOrder<'a>>, SQLError> {
    let mut retained = ProductionVec::new(control);
    for plan in scheduled {
        retained.push_copy(plan)?;
    }
    let mut primary = ProductionVec::new(control);
    let mut postponed = ProductionVec::new(control);
    if retained.iter().any(|cte| cte.body.modifies_data()) {
        let mut analysis = CteAnalysis::new(ctes, control)?;
        analysis.set_primary_names(primary_references)?;
        let read = analysis.closure(analysis.primary_indices())?;
        let mut deferred = ProductionVec::new(control);
        for _ in ctes {
            deferred.push_copy(false)?;
        }
        for (index, cte) in ctes.iter().enumerate().rev() {
            control.check()?;
            if !cte.body.modifies_data() || read[index] {
                continue;
            }
            let needed = analysis.closure(std::iter::once(index))?;
            for plan in retained.iter().copied() {
                control.check()?;
                if let Some(index) = analysis.index(&plan.name) {
                    if needed[index] && !read[index] && !deferred[index] {
                        postponed.push_copy(plan)?;
                        deferred.as_mut_slice()[index] = true;
                    }
                }
            }
        }
        for plan in retained.iter().copied() {
            control.check()?;
            if !analysis
                .index(&plan.name)
                .is_some_and(|index| deferred[index])
            {
                primary.push_copy(plan)?;
            }
        }
    } else {
        primary = retained;
    }
    let (primary, primary_memory) = primary.finish()?.into_parts();
    let (postponed, postponed_memory) = postponed.finish()?.into_parts();
    Ok(control.finish(
        StatementCteOrder { primary, postponed },
        control.combine(primary_memory, postponed_memory),
    )?)
}

#[cfg(test)]
mod tests;
