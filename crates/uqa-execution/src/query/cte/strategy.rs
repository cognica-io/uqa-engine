//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared CTE scheduling decisions for execution and traversal analysis.

use uqa_sql::{
    ast::CteMaterialization,
    plan::{CtePlan, QueryPlan},
    semantics::{
        cte_references_own_name, ordered_plan_ctes, reachable_plan_cte_names,
        single_reference_plan_cte_names,
        volatility::{query_contains_volatile_function, VolatilityCatalog},
    },
    SQLError,
};

pub(crate) struct ScheduledCte<'a> {
    pub plan: &'a CtePlan,
    pub deferred: bool,
}

pub(crate) fn schedule_plan_ctes<'a>(
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
