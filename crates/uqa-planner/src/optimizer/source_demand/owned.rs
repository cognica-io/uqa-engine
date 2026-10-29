//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional demand transfers mutable child owners through the original lexical and dependency order.

use super::{
    ctes, references, AggregateClassifier, Context, Demand, OptimizerConfig, RelationalPlan,
    SQLError,
};
use uqa_sql::plan::{MutableExecutablePlan, MutableQueryPlan, QueryChildSlot};

mod commands;
mod sources;

pub(in crate::optimizer) fn prune(
    plan: MutableExecutablePlan,
    config: &OptimizerConfig<'_>,
    aggregates: &dyn AggregateClassifier,
) -> Result<MutableExecutablePlan, SQLError> {
    Context {
        config,
        aggregates,
        ctes: None,
    }
    .statement_owned(plan)
}

impl Context<'_, '_> {
    fn output_context<'a>(
        &'a self,
        visible_ctes: &'a dyn Fn(&str) -> bool,
    ) -> super::super::OutputPruningContext<'a> {
        let routines = self.config.routine_inlining.as_ref();
        super::super::OutputPruningContext {
            production: self.config.production,
            aggregates: self.aggregates,
            routines: routines.map(|context| context.routines),
            volatility: routines.map(|context| context.volatility),
            visible_ctes,
        }
    }

    fn statement_owned(
        &self,
        plan: MutableExecutablePlan,
    ) -> Result<MutableExecutablePlan, SQLError> {
        Ok(match plan {
            MutableExecutablePlan::Query(query) => {
                MutableExecutablePlan::Query(self.query_owned(query)?)
            }
            MutableExecutablePlan::Command(command) => {
                MutableExecutablePlan::Command(self.command_owned(command)?)
            }
        })
    }

    fn query_owned(&self, mut query: MutableQueryPlan) -> Result<MutableQueryPlan, SQLError> {
        let control = &self.config.production;
        control.check()?;
        let (scope, order) = ctes::Scope::query(&query, self.config, self.ctes)?;
        let context = Context {
            config: self.config,
            aggregates: self.aggregates,
            ctes: Some(&scope),
        };
        match &query.root {
            RelationalPlan::QueryBlock(_) if query.analyzed_source.is_some() => {
                query = query.map_source_with_parent(control, |parent, source| {
                    let analysis = parent
                        .analyzed_source
                        .as_ref()
                        .expect("retained source analysis");
                    let RelationalPlan::QueryBlock(block) = &parent.root else {
                        unreachable!()
                    };
                    let mut demand = Demand::new(*control);
                    let mut references = references::References::new(&analysis.schema, &mut demand);
                    references::visit_query_inputs_with_control(
                        block,
                        control,
                        &mut |expression| references.expression(expression, &block.subqueries),
                    )?;
                    context.source_owned(source, analysis, &demand, &block.subqueries)
                })?;
            }
            RelationalPlan::SetOp { .. } => {
                query = query.map_query_child(QueryChildSlot::Left, control, |query| {
                    context.query_owned(query)
                })?;
                query = query.map_query_child(QueryChildSlot::Right, control, |query| {
                    context.query_owned(query)
                })?;
            }
            _ => {}
        }
        let count = match &query.root {
            RelationalPlan::QueryBlock(block) => block.subqueries.len(),
            RelationalPlan::SetOp { subqueries, .. }
            | RelationalPlan::Values { subqueries, .. } => subqueries.len(),
        };
        for index in 0..count {
            query = query.map_query_child(QueryChildSlot::Subquery(index), control, |query| {
                context.query_owned(query)
            })?;
        }
        context.cte_definitions_owned(query, &order)
    }

    fn cte_definitions_owned(
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
            query = query.map_cte(index, control, |body| match body {
                MutableExecutablePlan::Query(mut query) => {
                    if demand.observed.get() {
                        let retained = demand.positions.borrow();
                        if let Some(positions) = retained.as_ref() {
                            query = super::super::derived_projection::owned::prune_positions(
                                query,
                                &|position| positions.contains(position),
                                None,
                                self.output_context(&|name| self.visible_cte(name)),
                            )?;
                        }
                    }
                    Ok(MutableExecutablePlan::Query(self.query_owned(query)?))
                }
                MutableExecutablePlan::Command(command) => {
                    scope.retain_visible_values(*control)?;
                    Ok(MutableExecutablePlan::Command(self.command_owned(command)?))
                }
            })?;
        }
        Ok(query)
    }
}
