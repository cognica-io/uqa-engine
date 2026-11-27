//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Folded CTE demand consumes only the selected body and preserves declaration ownership.

use super::{AggregateClassifier, ProductionControl, SQLError, VolatilityCatalog};
use uqa_sql::plan::{MutableQueryPlan, QueryChildSlot};

pub(in crate::optimizer) fn prune(
    query: MutableQueryPlan,
    aggregates: &dyn AggregateClassifier,
    volatility: &dyn VolatilityCatalog,
    routines: Option<&dyn uqa_sql::routines::RoutineResolution>,
    production: ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    super::prune_folded(query, volatility, production, |query, index, required| {
        query.map_query_child_with_parent(
            QueryChildSlot::Cte(index),
            &production,
            |parent, body| {
                let (before, remaining) = parent.ctes.split_at(index);
                let (cte, after) = remaining.split_first().expect("scheduled CTE");
                let visible =
                    |name: &str| super::visible_from(name, before, &cte.name, cte.recursive, after);
                super::super::derived_projection::owned::prune_positions(
                    body,
                    required,
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
        )
    })
}
