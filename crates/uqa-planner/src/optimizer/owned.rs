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
    SQLError, ScalarExpr,
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
    let plan = super::derived_pullup::owned::rewrite(plan, config)?;
    let plan = plan.map_queries_while(control, &mut |query| {
        let query = query
            .prune_subqueries(control)?
            .discard_source_analysis(control)?;
        Ok((query, true))
    })?;
    Context {
        config,
        aggregates,
        active: &config.active_inline_routines,
    }
    .plan(plan)
}

pub(in crate::optimizer) struct Context<'a, 'scope> {
    pub config: &'a OptimizerConfig<'scope>,
    pub aggregates: &'a dyn AggregateClassifier,
    pub active: &'a [[u8; 16]],
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
        super::scalar::owned::optimize_with_active(expression, self.config, self.active)
    }

    fn qualification(&self, expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        let expression = self.scalar(expression)?;
        if self.config.enable_boolean_simplify {
            super::scalar::qualification::simplify_owned(
                expression,
                &self.config.production,
                &self.config.production,
            )
        } else {
            Ok(expression)
        }
    }

    fn access_predicate(&self, expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        if self.config.enable_filter_pushdown {
            super::access_path::owned::prioritize(expression, &self.config.production)
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
        let reachable =
            uqa_sql::semantics::reachable_query_cte_order_with_control(&query, *control)?;
        // Scalar initialization follows declaration order, even where CTE dependency analysis uses a different traversal order.
        for index in 0..query.ctes.len() {
            control.check()?;
            if reachable.contains(&index) {
                query = query.map_cte(index, control, |body| self.plan(body))?;
            }
        }
        drop(reachable);
        match &query.root {
            RelationalPlan::QueryBlock(_) => self.block(query),
            RelationalPlan::SetOp { .. } => {
                query = query
                    .map_query_child(QueryChildSlot::Left, control, |query| self.query(query))?;
                query = query
                    .map_query_child(QueryChildSlot::Right, control, |query| self.query(query))?;
                let RelationalPlan::SetOp { order_by, .. } = &query.root else {
                    unreachable!()
                };
                for index in 0..order_by.len() {
                    query = query.map_scalar(
                        QueryScalarSlot::OrderBy(index),
                        control,
                        |expression| self.scalar(expression),
                    )?;
                }
                query = query.map_scalar(QueryScalarSlot::Limit, control, |expression| {
                    self.scalar(expression)
                })?;
                query = query.map_scalar(QueryScalarSlot::Offset, control, |expression| {
                    self.scalar(expression)
                })?;
                let RelationalPlan::SetOp {
                    order_by,
                    limit,
                    offset,
                    ..
                } = &query.root
                else {
                    unreachable!()
                };
                let live = super::subqueries::live_slots(
                    order_by
                        .iter()
                        .map(|order| &order.expr)
                        .chain(limit.as_deref())
                        .chain(offset.as_deref()),
                    *control,
                )?;
                self.query_arena(query, &live)
            }
            RelationalPlan::Values { rows, .. } => {
                if rows.iter().any(|row| !row.is_empty()) {
                    let values = self.config.for_values_without_active()?;
                    let nested = Context {
                        config: &values,
                        aggregates: self.aggregates,
                        active: self.active,
                    };
                    let count = rows.len();
                    for row in 0..count {
                        let RelationalPlan::Values { rows, .. } = &query.root else {
                            unreachable!()
                        };
                        for column in 0..rows[row].len() {
                            query = query.map_scalar(
                                QueryScalarSlot::Value { row, column },
                                control,
                                |expression| nested.scalar(expression),
                            )?;
                        }
                    }
                }
                let RelationalPlan::Values { rows, .. } = &query.root else {
                    unreachable!()
                };
                let live = super::subqueries::live_slots(rows.iter().flatten(), *control)?;
                self.query_arena(query, &live)
            }
        }
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

fn projection_alias(
    projection: &super::ProjectionPlan,
    control: &ProductionControl<'_>,
) -> Result<Option<uqa_core::memory::Produced<String>>, SQLError> {
    if projection.alias.is_some()
        || matches!(
            projection.expr,
            ScalarExpr::Star | ScalarExpr::QualifiedStar(_)
        )
    {
        return Ok(None);
    }
    Ok(Some(uqa_sql::semantics::with_projection_label(
        projection,
        control,
        |name| Ok(control.copy_text(name)?),
    )?))
}
