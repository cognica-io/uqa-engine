//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native optimizer traversal keeps mutable constructor owners through every ordered pass.

use super::{AggregateClassifier, ComputePlan, OptimizerConfig, RelationalPlan, SourcePlan};
use uqa_core::memory::ProductionMap;
use uqa_sql::{
    plan::{
        MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan, MutableSourcePlan,
        OwnedScalarExpr, QueryChildSlot, QueryScalarSlot,
    },
    SQLError,
};

mod block;
mod commands;
mod demand;
mod sources;

pub(super) fn optimize_caller(
    plan: MutableExecutablePlan,
    config: &OptimizerConfig<'_>,
    aggregates: &dyn AggregateClassifier,
) -> Result<MutableExecutablePlan, SQLError> {
    Context::caller(config, aggregates).optimize(plan)
}

pub(super) fn optimize(
    plan: MutableExecutablePlan,
    config: &OptimizerConfig<'_>,
    aggregates: &dyn AggregateClassifier,
) -> Result<MutableExecutablePlan, SQLError> {
    Context::admitted(config, aggregates).optimize(plan)
}

pub(in crate::optimizer) struct Context<'a, 'scope> {
    pub config: &'a OptimizerConfig<'scope>,
    pub aggregates: &'a dyn AggregateClassifier,
    input: PlanInput,
}

#[derive(Clone, Copy)]
enum PlanInput {
    Caller,
    Admitted,
}

impl<'a, 'scope> Context<'a, 'scope> {
    pub(super) fn admitted(
        config: &'a OptimizerConfig<'scope>,
        aggregates: &'a dyn AggregateClassifier,
    ) -> Self {
        Self {
            config,
            aggregates,
            input: PlanInput::Admitted,
        }
    }

    pub(super) fn caller(
        config: &'a OptimizerConfig<'scope>,
        aggregates: &'a dyn AggregateClassifier,
    ) -> Self {
        Self {
            config,
            aggregates,
            input: PlanInput::Caller,
        }
    }

    fn optimize(&self, plan: MutableExecutablePlan) -> Result<MutableExecutablePlan, SQLError> {
        let storage = self.scalars().storage;
        let plan = plan.map_queries_while(&storage, &mut |query| {
            Ok((self.initialize_compute(query)?, true))
        })?;
        let plan = super::source_demand::prune(plan, self.config, self.aggregates, storage)?;
        self.optimize_sources(plan)
    }

    fn scalars(&self) -> super::scalar::ScalarProduction<'a, 'scope> {
        match self.input {
            PlanInput::Caller => super::scalar::ScalarProduction::caller(self.config),
            PlanInput::Admitted => super::scalar::ScalarProduction::admitted(self.config),
        }
    }

    fn initialize_compute(&self, query: MutableQueryPlan) -> Result<MutableQueryPlan, SQLError> {
        self.config.production.check()?;
        if query.analyzed_output.is_none() {
            if let RelationalPlan::QueryBlock(block) = &query.root {
                let compute =
                    block.compute_kind_with_control(self.aggregates, &self.config.production)?;
                return query.set_compute(compute, &self.scalars().storage);
            }
        }
        Ok(query)
    }

    // Source rewrites invalidate the analyzed layout before live CTEs and constants are planned.
    fn optimize_sources(
        &self,
        plan: MutableExecutablePlan,
    ) -> Result<MutableExecutablePlan, SQLError> {
        let storage = &self.scalars().storage;
        let plan = super::derived_pullup::rewrite(plan, self.config, storage)?;
        let plan = plan.map_queries_while(storage, &mut |query| {
            let query = query
                .prune_subqueries_with_workspace(storage, &self.config.production)?
                .discard_source_analysis(storage)?;
            Ok((query, true))
        })?;
        self.plan(plan)
    }

    pub(super) fn plan(
        &self,
        plan: MutableExecutablePlan,
    ) -> Result<MutableExecutablePlan, SQLError> {
        Ok(match plan {
            MutableExecutablePlan::Query(query) => MutableExecutablePlan::Query(self.query(query)?),
            MutableExecutablePlan::Command(command) => {
                MutableExecutablePlan::Command(self.command(command)?)
            }
        })
    }

    pub(in crate::optimizer) fn scalar(
        &self,
        expression: OwnedScalarExpr,
    ) -> Result<OwnedScalarExpr, SQLError> {
        super::scalar::owned::optimize_in(expression, self.scalars())
    }

    fn qualification(&self, expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        super::scalar::qualification::qualification(expression, self.scalars())
    }

    fn access_predicate(&self, expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        if self.config.enable_filter_pushdown {
            super::access_path::owned::prioritize(
                expression,
                &self.scalars().storage,
                &self.config.production,
            )
        } else {
            Ok(expression)
        }
    }

    pub(in crate::optimizer) fn query(
        &self,
        mut query: MutableQueryPlan,
    ) -> Result<MutableQueryPlan, SQLError> {
        let control = &self.config.production;
        let storage = &self.scalars().storage;
        control.check()?;
        query = query
            .apply_grouping(storage)?
            .prune_subqueries_with_workspace(storage, control)?;
        query = self.prune_cte_outputs(query)?;
        let reachable = uqa_sql::semantics::reachable_cte_declarations_with_control(
            uqa_sql::plan::PlanRef::Query(&query),
            *control,
        )?;
        for (index, &selected) in reachable.iter().enumerate() {
            control.check()?;
            if selected {
                query = query.map_cte(index, storage, |body| self.plan(body))?;
            }
        }
        drop(reachable);
        match &query.root {
            RelationalPlan::QueryBlock(_) => return self.block(query),
            RelationalPlan::SetOp { .. } => {
                query = query
                    .map_query_child(QueryChildSlot::Left, storage, |query| self.query(query))?;
                query = query
                    .map_query_child(QueryChildSlot::Right, storage, |query| self.query(query))?;
            }
            RelationalPlan::Values { .. } => {}
        }
        query = self.scalars().query_inputs(query)?;
        let live = super::subqueries::live_slots_for_query(&query, *control)?;
        self.query_arena(query, &live)
    }

    pub(super) fn query_arena(
        &self,
        mut query: MutableQueryPlan,
        live: &ProductionMap<'_, usize, ()>,
    ) -> Result<MutableQueryPlan, SQLError> {
        let count = match &query.root {
            RelationalPlan::QueryBlock(block) => block.subqueries.len(),
            RelationalPlan::SetOp { subqueries, .. }
            | RelationalPlan::Values { subqueries, .. } => subqueries.len(),
        };
        for (&index, ()) in live {
            self.config.production.check()?;
            if index < count {
                query = query.map_query_child(
                    QueryChildSlot::Subquery(index),
                    &self.scalars().storage,
                    |query| self.query(query),
                )?;
            }
        }
        Ok(query)
    }
}
