//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional output demand through analyzed join namespaces before scalar simplification.

mod references;

use std::collections::{BTreeMap, BTreeSet};

use uqa_sql::plan::AnalyzedSource;
use uqa_sql::{RowSchema, SQLError};

use super::OptimizerConfig;
use crate::{AggregateClassifier, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan};

type Demand = BTreeSet<usize>;

#[cfg(test)]
mod tests;

pub(super) fn prune(
    plan: &mut UnifiedPlan,
    config: &OptimizerConfig<'_>,
    aggregates: &dyn AggregateClassifier,
) -> Result<(), SQLError> {
    let mut context = Context {
        config,
        aggregates,
        ctes: Vec::new(),
    };
    let mut result = Ok(());
    plan.visit_query_roots_mut(&mut |query| {
        if result.is_ok() {
            result = context.query(query);
        }
    });
    result
}

struct Context<'a, 'catalog> {
    config: &'a OptimizerConfig<'catalog>,
    aggregates: &'a dyn AggregateClassifier,
    ctes: Vec<BTreeMap<String, CteDemand>>,
}

struct CteDemand {
    positions: Option<Demand>,
    observed: bool,
    visible: bool,
}

impl Context<'_, '_> {
    fn query(&mut self, query: &mut QueryPlan) -> Result<(), SQLError> {
        let order = self.push_ctes(query)?;
        let subqueries = match &mut query.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(analysis) = &query.analyzed_source {
                    let mut demand = Demand::new();
                    let mut references = references::References::new(&analysis.schema, &mut demand);
                    for expression in references::query_inputs(block) {
                        references.expression(expression, &block.subqueries);
                    }
                    if let Some(source) = &mut block.from {
                        self.source(source, analysis, &demand, &block.subqueries)?;
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
                self.query(left)?;
                self.query(right)?;
                subqueries
            }
            RelationalPlan::Values { subqueries, .. } => subqueries,
        };
        for query in subqueries {
            self.query(query)?;
        }
        self.cte_definitions(query, &order)?;
        self.ctes.pop();
        Ok(())
    }

    fn push_ctes(&mut self, query: &QueryPlan) -> Result<Vec<usize>, SQLError> {
        let folded =
            self.config
                .routine_inlining
                .as_ref()
                .map(|context| {
                    uqa_sql::semantics::cte_strategy::schedule_plan_ctes(context.volatility, query)
                        .map(|scheduled| {
                            scheduled
                                .into_iter()
                                .filter(|cte| cte.deferred)
                                .map(|cte| cte.plan.name.clone())
                                .collect::<BTreeSet<_>>()
                        })
                })
                .transpose()?
                .unwrap_or_default();
        self.ctes.push(
            query
                .ctes
                .iter()
                .map(|cte| {
                    (
                        cte.name.clone(),
                        CteDemand {
                            positions: folded.contains(&cte.name).then(Demand::new),
                            observed: false,
                            visible: true,
                        },
                    )
                })
                .collect(),
        );
        let positions = query
            .ctes
            .iter()
            .enumerate()
            .map(|(index, cte)| (cte.name.as_str(), index))
            .collect::<BTreeMap<_, _>>();
        Ok(uqa_sql::semantics::ordered_plan_ctes(query)?
            .iter()
            .map(|cte| positions[cte.name.as_str()])
            .collect())
    }

    fn cte_definitions(&mut self, query: &mut QueryPlan, order: &[usize]) -> Result<(), SQLError> {
        // Reverse dependency order propagates each consumer's demand before its producer is simplified, including forward references in WITH RECURSIVE.
        for &index in order.iter().rev() {
            let cte = &query.ctes[index];
            let scope = self.ctes.last_mut().expect("query CTE scope");
            let positions = scope
                .get(&cte.name)
                .filter(|demand| demand.observed)
                .and_then(|demand| demand.positions.clone());
            for (position, definition) in query.ctes.iter().enumerate() {
                scope
                    .get_mut(&definition.name)
                    .expect("CTE definition")
                    .visible = cte.recursive || position < index;
            }
            match &mut query.ctes[index].body {
                crate::CtePlanBody::Query(query) => {
                    if let Some(positions) = positions {
                        let routines = self.config.routine_inlining.as_ref();
                        super::derived_projection::prune_positions(
                            query,
                            &|position| positions.contains(&position),
                            None,
                            self.aggregates,
                            routines.map(|context| context.routines),
                            routines.map(|context| context.volatility),
                        );
                    }
                    self.query(query)?;
                }
                crate::CtePlanBody::Command(command) => {
                    // Command input layouts are not retained by query analysis. Preserve their visible CTE values until command demand has the same positional authority.
                    for scope in &mut self.ctes {
                        for demand in scope.values_mut().filter(|demand| demand.visible) {
                            demand.positions = None;
                        }
                    }
                    let mut result = Ok(());
                    command.visit_query_roots_mut(&mut |query| {
                        if result.is_ok() {
                            result = self.query(query);
                        }
                    });
                    result?;
                }
            }
        }
        Ok(())
    }

    fn source(
        &mut self,
        source: &mut SourcePlan,
        analysis: &AnalyzedSource,
        demand: &Demand,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        if let SourcePlan::Table { name, .. } = source {
            if let Some(name) = uqa_sql::semantics::cte_reference_name(name) {
                if let Some(selected) = self
                    .ctes
                    .iter_mut()
                    .rev()
                    .find_map(|scope| scope.get_mut(&name).filter(|demand| demand.visible))
                {
                    if let Some(positions) = &mut selected.positions {
                        selected.observed = true;
                        positions.extend((0..analysis.schema.len()).filter(|position| {
                            analysis
                                .schema
                                .slot(*position)
                                .is_some_and(|slot| demand.contains(&slot))
                        }));
                    }
                }
            }
        }
        match source {
            SourcePlan::Subquery {
                body,
                captured_output,
                ..
            } => {
                self.outputs(body, analysis, demand, *captured_output);
                self.query(body)
            }
            SourcePlan::Table {
                executable_query: Some(body),
                ..
            } => {
                self.outputs(body, analysis, demand, None);
                self.query(body)
            }
            SourcePlan::Join { .. } => self.join(source, analysis, demand, subqueries),
            _ => Ok(()),
        }
    }

    fn outputs(
        &self,
        query: &mut QueryPlan,
        analysis: &AnalyzedSource,
        demand: &Demand,
        captured: Option<uqa_sql::ast::InternalRelationId>,
    ) {
        let routines = self.config.routine_inlining.as_ref();
        super::derived_projection::prune_positions(
            query,
            &|position| {
                captured
                    .map_or_else(
                        || analysis.schema.slot(position),
                        |relation| analysis.schema.internal_slot(relation.column(position)),
                    )
                    .is_some_and(|slot| demand.contains(&slot))
            },
            None,
            self.aggregates,
            routines.map(|context| context.routines),
            routines.map(|context| context.volatility),
        );
    }

    fn join(
        &mut self,
        source: &mut SourcePlan,
        analysis: &AnalyzedSource,
        demanded: &Demand,
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
        let input = RowSchema::join(left_schema, right_schema, std::iter::empty());
        let mut demand = demanded.clone();
        let mut references = references::References::new(&input, &mut demand);
        if let Some(on) = on {
            references.expression(on, subqueries);
        }
        if let Some(using) = uqa_sql::semantics::resolve_join_using(
            using.as_ref(),
            *natural,
            left_schema,
            right_schema,
        )? {
            if let Some(predicate) =
                uqa_sql::semantics::join_using_predicate(&using, left_schema, right_schema)
            {
                references.expression(&predicate, subqueries);
            }
        }
        let boundary = left_schema.physical_width();
        let mut left_demand = demand
            .iter()
            .copied()
            .filter(|slot| *slot < boundary)
            .collect::<Demand>();
        let right_demand = demand
            .iter()
            .filter_map(|slot| slot.checked_sub(boundary))
            .collect::<Demand>();
        self.source(right, &children.1, &right_demand, subqueries)?;
        // Only expressions still consumed by the right input can demand a preceding value. A discarded LATERAL output must not keep its producer's unused error alive.
        references::References::new(left_schema, &mut left_demand).source(
            right,
            Some(&children.1),
            subqueries,
            &[],
            *lateral,
        );
        self.source(left, &children.0, &left_demand, subqueries)
    }
}
