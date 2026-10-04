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
        cte_definition_references, cte_references_own_name, ordered_plan_ctes,
        reachable_plan_cte_names, single_reference_plan_cte_names,
        volatility::{query_contains_volatile_function, VolatilityCatalog},
    },
    SQLError,
};
use std::collections::{BTreeMap, BTreeSet};

pub struct ScheduledCte<'a> {
    pub plan: &'a CtePlan,
    pub deferred: bool,
}

pub fn schedule_plan_ctes<'a>(
    catalog: &dyn VolatilityCatalog,
    plan: &'a QueryPlan,
) -> Result<Vec<ScheduledCte<'a>>, SQLError> {
    let ordered = ordered_plan_ctes(plan)?;
    let reachable = reachable_plan_cte_names(plan);
    let single_reference = single_reference_plan_cte_names(plan);
    Ok(ordered
        .into_iter()
        .filter(|cte| reachable.contains(&cte.name))
        .map(|cte| ScheduledCte {
            plan: cte,
            deferred: !cte.body.modifies_data()
                && !cte_references_own_name(cte)
                && match cte.materialization {
                    CteMaterialization::Default => single_reference.contains(&cte.name),
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
                ),
        })
        .collect())
}

/// The materialized WITH items of a statement, split by when they run. `PostgreSQL` runs a data-modifying item while its reader draws rows from it, and once the primary query has finished `ExecPostprocessPlan` runs every item no reader drew from to completion, the last defined first.
pub struct StatementCteOrder<'a> {
    /// The items that run before the primary query: the ones it reads, directly or through other items, and the queries that no postponed item reads.
    pub primary: Vec<&'a CtePlan>,
    /// The items that run once the primary query has finished, each data-modifying item after the items it reads.
    pub postponed: Vec<&'a CtePlan>,
}

/// Split `scheduled`, the items of `ctes` that a statement materializes in dependency order, into the items that run before its primary query and the items that run after it. `primary_references` names the items the primary query reads.
pub fn order_statement_ctes<'a>(
    ctes: &'a [CtePlan],
    scheduled: Vec<&'a CtePlan>,
    primary_references: &BTreeSet<String>,
) -> StatementCteOrder<'a> {
    if !scheduled.iter().any(|cte| cte.body.modifies_data()) {
        return StatementCteOrder {
            primary: scheduled,
            postponed: Vec::new(),
        };
    }
    let references = ctes
        .iter()
        .enumerate()
        .map(|(index, cte)| (cte.name.as_str(), cte_definition_references(ctes, index)))
        .collect::<BTreeMap<_, _>>();
    // The items `roots` read, directly or through the items they read, with the roots themselves.
    let read_through = |roots: Vec<String>| {
        let mut reached = BTreeSet::new();
        let mut pending = roots;
        while let Some(name) = pending.pop() {
            if let Some(names) = references.get(name.as_str()) {
                pending.extend(
                    names
                        .iter()
                        .filter(|name| !reached.contains(*name))
                        .cloned(),
                );
            }
            reached.insert(name);
        }
        reached
    };
    let read = read_through(primary_references.iter().cloned().collect());
    let mut postponed_names = BTreeSet::new();
    let mut postponed = Vec::new();
    for item in ctes
        .iter()
        .rev()
        .filter(|cte| cte.body.modifies_data() && !read.contains(&cte.name))
    {
        let needed = read_through(vec![item.name.clone()]);
        for cte in &scheduled {
            if needed.contains(&cte.name)
                && !read.contains(&cte.name)
                && postponed_names.insert(cte.name.clone())
            {
                postponed.push(*cte);
            }
        }
    }
    let primary = scheduled
        .into_iter()
        .filter(|cte| !postponed_names.contains(&cte.name))
        .collect();
    StatementCteOrder { primary, postponed }
}

#[cfg(test)]
mod tests;
