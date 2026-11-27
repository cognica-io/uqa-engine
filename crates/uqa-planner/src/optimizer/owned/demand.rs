//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection pruning borrows ordinary query fields or transfers their admitted constructors.

use super::{Context, MutableQueryPlan, SQLError};

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
        super::super::cte_projection::prune_plan(
            query,
            self.aggregates,
            routines.volatility,
            Some(routines.routines),
            self.config.production,
            self.scalars().storage,
        )
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
        super::super::derived_projection::owned::prune(query, demand, self.scalars().storage)
    }
}
