//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional demand through analyzed join namespaces before scalar simplification.

mod commands;
mod ctes;
mod positions;
mod references;
mod sources;
#[cfg(test)]
mod tests;

use super::{OptimizerConfig, OutputPruningContext};
use crate::{AggregateClassifier, QueryPlan, RelationalPlan, SourcePlan};
use positions::Demand;
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity, RelationName,
};
use uqa_sql::{
    plan::{MutableExecutablePlan, MutableQueryPlan, QueryChildSlot},
    SQLError,
};

pub(super) fn prune<'catalog>(
    plan: MutableExecutablePlan,
    config: &OptimizerConfig<'catalog>,
    aggregates: &dyn AggregateClassifier,
    storage: ProductionControl<'catalog>,
) -> Result<MutableExecutablePlan, SQLError> {
    Context {
        config,
        aggregates,
        storage,
        ctes: None,
    }
    .statement(plan)
}

struct Context<'a, 'catalog> {
    config: &'a OptimizerConfig<'catalog>,
    aggregates: &'a dyn AggregateClassifier,
    storage: ProductionControl<'catalog>,
    ctes: Option<&'a ctes::Scope<'a, 'catalog>>,
}

struct QueryDemand<'a, 'catalog> {
    scope: ctes::Scope<'a, 'catalog>,
    order: Produced<Vec<usize>>,
    source: Option<Demand<'catalog>>,
}

impl<'a, 'catalog> Context<'a, 'catalog> {
    fn visible_cte(&self, name: &str) -> bool {
        self.ctes
            .is_some_and(|scope| scope.find(RelationName::decoded(name)).is_some())
    }

    fn referenced_cte(&self, reference: &str) -> Option<&ctes::CteDemand<'catalog>> {
        let reference = RelationIdentity::borrowed_reference(reference)?;
        if reference.schema.is_some() {
            return None;
        }
        self.ctes?.find(reference.name)
    }

    fn prune_positions(
        &self,
        query: MutableQueryPlan,
        required: &dyn Fn(usize) -> bool,
    ) -> Result<MutableQueryPlan, SQLError> {
        let routines = self.config.routine_inlining.as_ref();
        let visible_ctes = |name: &str| self.visible_cte(name);
        let context = OutputPruningContext {
            production: self.config.production,
            aggregates: self.aggregates,
            routines: routines.map(|scope| scope.routines),
            volatility: routines.map(|scope| scope.volatility),
            visible_ctes: &visible_ctes,
        };
        super::derived_projection::owned::prune_plan_positions(
            query,
            required,
            None,
            context,
            self.storage,
        )
    }

    fn statement(&self, plan: MutableExecutablePlan) -> Result<MutableExecutablePlan, SQLError> {
        self.config.production.check()?;
        Ok(match plan {
            MutableExecutablePlan::Query(query) => MutableExecutablePlan::Query(self.query(query)?),
            MutableExecutablePlan::Command(command) => {
                MutableExecutablePlan::Command(self.command(command)?)
            }
        })
    }

    // CTE scheduling and wildcard or qualified references can fail before any source ownership moves.
    fn query_demand(&self, query: &QueryPlan) -> Result<QueryDemand<'a, 'catalog>, SQLError> {
        let control = self.config.production;
        control.check()?;
        let (scope, order) = ctes::Scope::query(query, self.config, self.ctes)?;
        let source = match (&query.root, &query.analyzed_source) {
            (RelationalPlan::QueryBlock(block), Some(analysis)) => {
                let mut demand = Demand::new(control);
                let mut references = references::References::new(&analysis.schema, &mut demand);
                references::visit_query_inputs_with_control(block, &control, &mut |expr| {
                    references.expression(expr, &block.subqueries)
                })?;
                Some(demand)
            }
            _ => None,
        };
        Ok(QueryDemand {
            scope,
            order,
            source,
        })
    }

    fn query(&self, mut query: MutableQueryPlan) -> Result<MutableQueryPlan, SQLError> {
        let QueryDemand {
            scope,
            order,
            source,
        } = self.query_demand(&query)?;
        let context = Context {
            ctes: Some(&scope),
            ..*self
        };
        if let Some(demand) = source {
            query = query.map_source_with_parent(&self.storage, |parent, source| {
                let analysis = parent
                    .analyzed_source
                    .as_ref()
                    .expect("analyzed query input");
                let RelationalPlan::QueryBlock(block) = &parent.root else {
                    unreachable!("query block demand")
                };
                context.source(source, analysis, &demand, &block.subqueries)
            })?;
        } else if matches!(query.root, RelationalPlan::SetOp { .. }) {
            query =
                query.map_query_child(QueryChildSlot::Left, &self.storage, |q| context.query(q))?;
            query = query
                .map_query_child(QueryChildSlot::Right, &self.storage, |q| context.query(q))?;
        }
        let count = match &query.root {
            RelationalPlan::QueryBlock(block) => block.subqueries.len(),
            RelationalPlan::SetOp { subqueries, .. }
            | RelationalPlan::Values { subqueries, .. } => subqueries.len(),
        };
        for index in 0..count {
            query = query.map_query_child(QueryChildSlot::Subquery(index), &self.storage, |q| {
                context.query(q)
            })?;
        }
        context.cte_definitions(query, &order)
    }

    fn cte_definitions(
        &self,
        mut query: MutableQueryPlan,
        order: &[usize],
    ) -> Result<MutableQueryPlan, SQLError> {
        let control = &self.config.production;
        for &index in order.iter().rev() {
            control.check()?;
            let cte = &query.ctes[index];
            let scope = self.ctes.expect("query CTE scope");
            let demand = scope.local(&cte.name);
            scope.set_visible(&query.ctes, index, cte.recursive, *control)?;
            query = query.map_cte(index, &self.storage, |body| match body {
                MutableExecutablePlan::Query(mut query) => {
                    if demand.observed.get() {
                        let retained = demand.positions.borrow();
                        if let Some(positions) = retained.as_ref() {
                            query = self
                                .prune_positions(query, &|position| positions.contains(position))?;
                        }
                    }
                    Ok(MutableExecutablePlan::Query(self.query(query)?))
                }
                MutableExecutablePlan::Command(command) => {
                    scope.retain_visible_values(*control)?;
                    Ok(MutableExecutablePlan::Command(self.command(command)?))
                }
            })?;
        }
        Ok(query)
    }
}
