//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection pruning borrows ordinary query fields or transfers their admitted constructors.

use super::{Context, MutableQueryPlan, PlanInput, RelationalPlan, SQLError};
use crate::QueryPlan;
use uqa_core::memory::ProductionControl;

impl Context<'_, '_> {
    pub(super) fn prune_cte_outputs(
        &self,
        query: MutableQueryPlan,
    ) -> Result<MutableQueryPlan, SQLError> {
        let Some(routines) = self
            .config
            .routine_inlining
            .as_ref()
            .filter(|_| query.analyzed_output.is_none())
        else {
            return Ok(query);
        };
        match self.input {
            PlanInput::Caller => edit_ordinary(query, |query| {
                super::super::cte_projection::prune(
                    query,
                    self.aggregates,
                    routines.volatility,
                    Some(routines.routines),
                    self.config.production,
                )
            }),
            PlanInput::Admitted => super::super::cte_projection::owned::prune(
                query,
                self.aggregates,
                routines.volatility,
                Some(routines.routines),
                self.config.production,
            ),
        }
    }

    pub(super) fn prune_derived_outputs(
        &self,
        query: MutableQueryPlan,
    ) -> Result<MutableQueryPlan, SQLError> {
        if query.analyzed_output.is_some() {
            return Ok(query);
        }
        let routines = self.config.routine_inlining.as_ref();
        let demand = super::super::OutputPruningContext {
            production: self.config.production,
            aggregates: self.aggregates,
            routines: routines.map(|scope| scope.routines),
            volatility: routines.map(|scope| scope.volatility),
            visible_ctes: &|_| false,
        };
        match self.input {
            PlanInput::Caller => edit_ordinary(query, |query| {
                let RelationalPlan::QueryBlock(block) = &mut query.root else {
                    unreachable!("query block demand");
                };
                super::super::derived_projection::prune(block, demand)
            }),
            PlanInput::Admitted => super::super::derived_projection::owned::prune(query, demand),
        }
    }
}

fn edit_ordinary(
    query: MutableQueryPlan,
    edit: impl FnOnce(&mut QueryPlan) -> Result<(), SQLError>,
) -> Result<MutableQueryPlan, SQLError> {
    let mut query = query
        .finish(&ProductionControl::uncontrolled())?
        .into_uncontrolled()
        .expect("ordinary projection input");
    edit(&mut query)?;
    Ok(MutableQueryPlan::uncontrolled(query))
}
