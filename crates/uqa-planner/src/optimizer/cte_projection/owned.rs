//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Folded CTE demand consumes only the selected body and preserves declaration ownership.

use super::{
    demand, AggregateClassifier, ProductionControl, RelationalPlan, SQLError, VolatilityCatalog,
};
use uqa_sql::plan::{MutableQueryPlan, QueryChildSlot};

pub(in crate::optimizer) fn prune(
    mut query: MutableQueryPlan,
    aggregates: &dyn AggregateClassifier,
    volatility: &dyn VolatilityCatalog,
    routines: Option<&dyn uqa_sql::routines::RoutineResolution>,
    production: ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let folded = super::folded_ctes(&query, volatility, production)?;
    for index in folded.iter().copied() {
        production.check()?;
        let cte = &query.ctes[index];
        let Some(body) = cte.body.query() else {
            continue;
        };
        let RelationalPlan::QueryBlock(block) = &body.root else {
            continue;
        };
        let names = uqa_sql::semantics::projection_labels_with_control(
            &block.projections,
            &cte.columns,
            &production,
        )?;
        let mut demand = demand::Demand::new(&cte.name, &names, production);
        demand.collect_root(&query)?;
        for other in &query.ctes {
            production.check()?;
            if other.name == cte.name {
                continue;
            }
            if let Some(body) = other.body.query() {
                demand.collect_query(body)?;
            } else {
                demand.all = true;
            }
        }
        if demand.all {
            continue;
        }
        let demanded = super::super::derived_projection::output_demand(
            body,
            &cte.columns,
            &|column| demand.required(column),
            None,
            production,
        )?
        .expect("query CTE output descriptor");
        drop(demand);
        drop(names);
        query = query.map_query_child_with_parent(
            QueryChildSlot::Cte(index),
            &production,
            |parent, body| {
                let (before, remaining) = parent.ctes.split_at(index);
                let (cte, after) = remaining.split_first().expect("scheduled CTE");
                let visible = |name: &str| {
                    before.iter().any(|cte| cte.name == name)
                        || (cte.recursive
                            && (cte.name == name || after.iter().any(|cte| cte.name == name)))
                };
                super::super::derived_projection::owned::prune_positions(
                    body,
                    &|position| demanded.get(position).copied().unwrap_or(true),
                    None,
                    super::super::OutputPruningContext {
                        production,
                        aggregates,
                        routines,
                        volatility: Some(volatility),
                        visible_ctes: &visible,
                    },
                )
            },
        )?;
    }
    production.check()?;
    Ok(query)
}
