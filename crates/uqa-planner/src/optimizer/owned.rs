//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native optimizer traversal keeps mutable constructor owners through every ordered pass.

use super::{AggregateClassifier, ComputePlan, OptimizerConfig, RelationalPlan, SourcePlan};
use uqa_core::memory::{ProductionControl, ProductionMap};
use uqa_sql::{
    plan::{
        MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan, MutableSourcePlan,
        OwnedScalarExpr, QueryChildSlot, QueryScalarSlot,
    },
    SQLError,
};

mod block;
mod commands;
mod sources;

pub(super) fn optimize(
    plan: MutableExecutablePlan,
    config: &OptimizerConfig<'_>,
    aggregates: &dyn AggregateClassifier,
) -> Result<MutableExecutablePlan, SQLError> {
    let control = &config.production;
    let plan = plan.map_queries_while(control, &mut |mut query| {
        if query.analyzed_output.is_none() {
            if let RelationalPlan::QueryBlock(block) = &query.root {
                let compute = block.compute_kind_with_control(aggregates, control)?;
                query = query.set_compute(compute, control)?;
            }
        }
        Ok((query, true))
    })?;
    let plan = super::source_demand::owned::prune(plan, config, aggregates)?;
    let plan = rewrite_sources(plan, config, control)?;
    Context { config, aggregates }.plan(plan)
}

/// Source rewrites invalidate the analyzed layout. Compact discarded subqueries before planning live CTEs and evaluating constants.
pub(super) fn rewrite_sources(
    plan: MutableExecutablePlan,
    config: &OptimizerConfig<'_>,
    storage: &ProductionControl<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    let plan = super::derived_pullup::rewrite(plan, config, storage)?;
    plan.map_queries_while(storage, &mut |query| {
        let query = query
            .prune_subqueries_with_workspace(storage, &config.production)?
            .discard_source_analysis(storage)?;
        Ok((query, true))
    })
}

pub(in crate::optimizer) struct Context<'a, 'scope> {
    pub config: &'a OptimizerConfig<'scope>,
    pub aggregates: &'a dyn AggregateClassifier,
}

impl Context<'_, '_> {
    fn plan(&self, plan: MutableExecutablePlan) -> Result<MutableExecutablePlan, SQLError> {
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
        super::scalar::owned::optimize(expression, self.config)
    }

    fn qualification(&self, expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        super::scalar::optimize_qualification_owned(expression, self.config)
    }

    fn access_predicate(&self, expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        if self.config.enable_filter_pushdown {
            super::access_path::owned::prioritize(
                expression,
                &self.config.production,
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
        query = query.apply_grouping(control)?.prune_subqueries(control)?;
        if query.analyzed_output.is_none() {
            if let Some(routines) = &self.config.routine_inlining {
                query = super::cte_projection::owned::prune(
                    query,
                    self.aggregates,
                    routines.volatility,
                    Some(routines.routines),
                    *control,
                )?;
            }
        }
        let reachable = uqa_sql::semantics::reachable_cte_declarations_with_control(
            uqa_sql::plan::PlanRef::Query(&query),
            *control,
        )?;
        for (index, &selected) in reachable.iter().enumerate() {
            control.check()?;
            if selected {
                query = query.map_cte(index, control, |body| self.plan(body))?;
            }
        }
        drop(reachable);
        match &query.root {
            RelationalPlan::QueryBlock(_) => return self.block(query),
            RelationalPlan::SetOp { .. } => {
                query = query
                    .map_query_child(QueryChildSlot::Left, control, |query| self.query(query))?;
                query = query
                    .map_query_child(QueryChildSlot::Right, control, |query| self.query(query))?;
            }
            RelationalPlan::Values { .. } => {}
        }
        query = super::scalar::ScalarProduction::admitted(self.config).query_inputs(query)?;
        let live = super::subqueries::live_slots_for_query(&query, *control)?;
        self.query_arena(query, &live)
    }

    fn query_arena(
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
                    &self.config.production,
                    |query| self.query(query),
                )?;
            }
        }
        Ok(query)
    }
}
