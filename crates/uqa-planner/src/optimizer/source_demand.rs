//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional output demand through analyzed join namespaces before scalar simplification.

mod ctes;
mod positions;
mod references;

pub(super) mod owned;

use positions::Demand;
use uqa_core::{RelationIdentity, RelationName};

use uqa_sql::plan::AnalyzedSource;
use uqa_sql::SQLError;

use super::OptimizerConfig;
use crate::{AggregateClassifier, CommandPlan, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan};

#[cfg(test)]
mod tests;

pub(super) fn prune(
    plan: &mut UnifiedPlan,
    config: &OptimizerConfig<'_>,
    aggregates: &dyn AggregateClassifier,
) -> Result<(), SQLError> {
    let context = Context {
        config,
        aggregates,
        ctes: None,
    };
    match plan {
        UnifiedPlan::Query(query) => context.query(query),
        UnifiedPlan::Command(command) => context.command(command),
    }
}

struct Context<'a, 'catalog> {
    config: &'a OptimizerConfig<'catalog>,
    aggregates: &'a dyn AggregateClassifier,
    ctes: Option<&'a ctes::Scope<'a, 'catalog>>,
}

impl<'catalog> Context<'_, 'catalog> {
    fn command(&self, command: &mut CommandPlan) -> Result<(), SQLError> {
        if !matches!(
            command,
            CommandPlan::Insert(_)
                | CommandPlan::Update(_)
                | CommandPlan::Delete(_)
                | CommandPlan::Merge(_)
        ) {
            let mut result = Ok(());
            command.visit_query_roots_mut(&mut |query| {
                if result.is_ok() {
                    result = self.query(query);
                }
            });
            return result;
        }
        let scope = if command.ctes().is_empty() {
            None
        } else {
            Some(ctes::Scope::command(
                command.ctes(),
                self.config.production,
                self.ctes,
            )?)
        };
        let context = Context {
            config: self.config,
            aggregates: self.aggregates,
            ctes: scope.as_ref().or(self.ctes),
        };
        if let Some(ctes) = command.ctes_mut() {
            for index in 0..ctes.len() {
                self.config.production.check()?;
                context.ctes.expect("command CTE scope").set_visible(
                    ctes,
                    index,
                    ctes[index].recursive,
                    self.config.production,
                )?;
                match &mut ctes[index].body {
                    crate::CtePlanBody::Query(query) => context.query(query)?,
                    crate::CtePlanBody::Command(command) => context.command(command)?,
                }
            }
        }
        if let Some(scope) = &scope {
            scope.set_visible(
                command.ctes(),
                command.ctes().len(),
                true,
                self.config.production,
            )?;
        }
        for query in command.query_inputs_mut() {
            context.query(query)?;
        }
        if let Some(source) = command.source_input_mut() {
            context.source_queries(source)?;
        }
        Ok(())
    }

    fn source_queries(&self, source: &mut SourcePlan) -> Result<(), SQLError> {
        match source {
            SourcePlan::Subquery { body, .. } => self.query(body)?,
            SourcePlan::Table {
                name,
                executable_query: Some(body),
                ..
            } => {
                if self.referenced_cte(name).is_some() {
                    self.query(body)?;
                } else {
                    Context {
                        config: self.config,
                        aggregates: self.aggregates,
                        ctes: None,
                    }
                    .query(body)?;
                }
            }
            SourcePlan::Join { left, right, .. } => {
                self.source_queries(left)?;
                self.source_queries(right)?;
            }
            _ => {}
        }
        Ok(())
    }

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

    fn query(&self, query: &mut QueryPlan) -> Result<(), SQLError> {
        self.config.production.check()?;
        let (scope, order) = ctes::Scope::query(query, self.config, self.ctes)?;
        let context = Context {
            config: self.config,
            aggregates: self.aggregates,
            ctes: Some(&scope),
        };
        let subqueries = match &mut query.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(analysis) = &query.analyzed_source {
                    let mut demand = Demand::new(self.config.production);
                    let mut references = references::References::new(&analysis.schema, &mut demand);
                    references::visit_query_inputs_with_control(
                        block,
                        &self.config.production,
                        &mut |expression| references.expression(expression, &block.subqueries),
                    )?;
                    if let Some(source) = &mut block.from {
                        context.source(source, analysis, &demand, &block.subqueries)?;
                    }
                }
                &mut block.subqueries
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                context.query(left)?;
                context.query(right)?;
                subqueries
            }
            RelationalPlan::Values { subqueries, .. } => subqueries,
        };
        for query in subqueries {
            context.query(query)?;
        }
        context.cte_definitions(query, &order)?;
        Ok(())
    }

    fn cte_definitions(&self, query: &mut QueryPlan, order: &[usize]) -> Result<(), SQLError> {
        // Reverse dependency order propagates consumer demand before producer simplification, including forward references in WITH RECURSIVE.
        for &index in order.iter().rev() {
            self.config.production.check()?;
            let cte = &query.ctes[index];
            let scope = self.ctes.expect("query CTE scope");
            let demand = scope.local(&cte.name);
            scope.set_visible(&query.ctes, index, cte.recursive, self.config.production)?;
            match &mut query.ctes[index].body {
                crate::CtePlanBody::Query(query) => {
                    if demand.observed.get() {
                        let retained = demand.positions.borrow();
                        if let Some(positions) = retained.as_ref() {
                            let routines = self.config.routine_inlining.as_ref();
                            super::derived_projection::prune_positions(
                                query,
                                &|position| positions.contains(position),
                                None,
                                super::OutputPruningContext {
                                    production: self.config.production,
                                    aggregates: self.aggregates,
                                    routines: routines.map(|context| context.routines),
                                    volatility: routines.map(|context| context.volatility),
                                    visible_ctes: &|name| self.visible_cte(name),
                                },
                            )?;
                        }
                    }
                    self.query(query)?;
                }
                crate::CtePlanBody::Command(command) => {
                    // Command input layouts are not retained by query analysis. Preserve visible CTE values until command demand has the same positional authority.
                    scope.retain_visible_values(self.config.production)?;
                    self.command(command)?;
                }
            }
        }
        Ok(())
    }

    fn source(
        &self,
        source: &mut SourcePlan,
        analysis: &AnalyzedSource,
        demand: &Demand<'_>,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        self.config.production.check()?;
        if let SourcePlan::Table { name, .. } = source {
            if let Some(selected) = self.referenced_cte(name) {
                if let Some(positions) = &mut *selected.positions.borrow_mut() {
                    selected.observed.set(true);
                    positions.extend((0..analysis.schema.len()).filter(|position| {
                        analysis
                            .schema
                            .slot(*position)
                            .is_some_and(|slot| demand.contains(slot))
                    }))?;
                }
            }
        }
        match source {
            SourcePlan::Subquery {
                body,
                captured_output,
                ..
            } => {
                self.outputs(body, analysis, demand, *captured_output)?;
                self.query(body)
            }
            SourcePlan::Table {
                name,
                executable_query: Some(body),
                ..
            } => {
                if self.referenced_cte(name).is_some() {
                    self.outputs(body, analysis, demand, None)?;
                    self.query(body)
                } else {
                    let context = Context {
                        config: self.config,
                        aggregates: self.aggregates,
                        ctes: None,
                    };
                    context.outputs(body, analysis, demand, None)?;
                    context.query(body)
                }
            }
            SourcePlan::Join { .. } => self.join(source, analysis, demand, subqueries),
            _ => Ok(()),
        }
    }

    fn outputs(
        &self,
        query: &mut QueryPlan,
        analysis: &AnalyzedSource,
        demand: &Demand<'_>,
        captured: Option<uqa_sql::ast::InternalRelationId>,
    ) -> Result<(), SQLError> {
        let routines = self.config.routine_inlining.as_ref();
        super::derived_projection::prune_positions(
            query,
            &|position| {
                captured
                    .map_or_else(
                        || analysis.schema.slot(position),
                        |relation| analysis.schema.internal_slot(relation.column(position)),
                    )
                    .is_some_and(|slot| demand.contains(slot))
            },
            None,
            super::OutputPruningContext {
                production: self.config.production,
                aggregates: self.aggregates,
                routines: routines.map(|context| context.routines),
                volatility: routines.map(|context| context.volatility),
                visible_ctes: &|name| self.visible_cte(name),
            },
        )
    }

    fn join(
        &self,
        source: &mut SourcePlan,
        analysis: &AnalyzedSource,
        demanded: &Demand<'_>,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        let SourcePlan::Join {
            left,
            right,
            on,
            using,
            natural,
            lateral,
            ..
        } = source
        else {
            unreachable!("join source");
        };
        let Some(children) = &analysis.children else {
            return Ok(());
        };
        let (left_schema, right_schema) = (&children.0.schema, &children.1.schema);
        let mut demand = demanded.copy()?;
        if let Some(on) = on {
            references::References::join(left_schema, right_schema, &mut demand)?
                .expression(on, subqueries)?;
        }
        if let Some(using) = uqa_sql::semantics::resolve_join_using_with_control(
            using.as_ref(),
            *natural,
            left_schema,
            right_schema,
            &self.config.production,
        )? {
            using.visit_input_slots(
                left_schema,
                right_schema,
                &self.config.production,
                &mut |slot| demand.extend([slot]),
            )?;
        }
        let boundary = left_schema.physical_width();
        let mut left_demand = Demand::new(self.config.production);
        left_demand.extend(demand.iter().copied().filter(|slot| *slot < boundary))?;
        let mut right_demand = Demand::new(self.config.production);
        right_demand.extend(demand.iter().filter_map(|slot| slot.checked_sub(boundary)))?;
        self.source(right, &children.1, &right_demand, subqueries)?;
        // Only expressions still consumed by the right input can demand a preceding value. A discarded LATERAL output must not keep its producer's unused error alive.
        references::References::new(left_schema, &mut left_demand).source(
            right,
            Some(&children.1),
            subqueries,
            &[],
            *lateral,
        )?;
        self.source(left, &children.0, &left_demand, subqueries)
    }
}
