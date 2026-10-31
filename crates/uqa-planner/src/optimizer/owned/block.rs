//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-block rewrites preserve original scalar, grouping and live-subquery order.

use super::{ComputePlan, Context, MutableQueryPlan, QueryScalarSlot, RelationalPlan, SQLError};

impl Context<'_, '_> {
    pub(super) fn block(&self, mut query: MutableQueryPlan) -> Result<MutableQueryPlan, SQLError> {
        let control = &self.config.production;
        let routines = self.config.routine_inlining.as_ref();
        if query.analyzed_output.is_none() {
            query = super::super::derived_projection::owned::prune(
                query,
                super::super::OutputPruningContext {
                    production: *control,
                    aggregates: self.aggregates,
                    routines: routines.map(|context| context.routines),
                    volatility: routines.map(|context| context.volatility),
                    visible_ctes: &|_| false,
                },
            )?;
        }
        query = query.map_source(control, |source| self.source(source))?;
        query = super::super::source_constants::owned::propagate(
            query,
            routines.map(|context| context.types),
            routines.map(|context| context.volatility),
            control,
        )?;
        query = self.projections(query)?;
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        let allow_unqualified =
            super::super::source_allows_unqualified_signals(block.from.as_ref());
        query = query.map_scalar(QueryScalarSlot::Where, control, |expression| {
            let expression = self.qualification(expression)?;
            let expression = super::super::implicit_fusion::owned::rewrite(
                expression,
                allow_unqualified,
                control,
            )?;
            self.access_predicate(expression)
        })?;
        query = self.grouping(query)?;
        query = query.map_scalar(QueryScalarSlot::Having, control, |expression| {
            self.qualification(expression)
        })?;
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        for index in 0..block.order_by.len() {
            query = query.map_scalar(QueryScalarSlot::OrderBy(index), control, |expression| {
                self.scalar(expression)
            })?;
        }
        query = query.map_scalar(QueryScalarSlot::Limit, control, |expression| {
            self.scalar(expression)
        })?;
        query = query.map_scalar(QueryScalarSlot::Offset, control, |expression| {
            self.scalar(expression)
        })?;
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        for index in 0..block.distinct_on.len() {
            query =
                query.map_scalar(QueryScalarSlot::DistinctOn(index), control, |expression| {
                    self.scalar(expression)
                })?;
        }
        query = self.windows(query)?;
        query = super::super::source_constants::owned::simplify_group(
            query,
            routines.map(|context| context.types),
            control,
        )?;
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        let live = super::super::subqueries::live_slots_for_block(block, *control)?;
        query = self.query_arena(query, &live)?;
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        // A removed aggregate value still determines whether empty input produces one implicit group.
        if !matches!(block.compute, ComputePlan::Aggregate) {
            let compute = block.compute_kind_with_control(self.aggregates, control)?;
            query = query.set_compute(compute, control)?;
        }
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        let access = super::super::choose_access_path(block);
        query.set_access(access, control)
    }

    fn projections(&self, mut query: MutableQueryPlan) -> Result<MutableQueryPlan, SQLError> {
        let control = &self.config.production;
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        let count = block.projections.len();
        for index in 0..count {
            let RelationalPlan::QueryBlock(block) = &query.root else {
                unreachable!()
            };
            if let Some(alias) = super::projection_alias(&block.projections[index], control)? {
                query = query.replace_projection_alias(index, Some(alias), control)?;
            }
            query =
                query.map_scalar(QueryScalarSlot::Projection(index), control, |expression| {
                    self.scalar(expression)
                })?;
        }
        Ok(query)
    }

    fn grouping(&self, mut query: MutableQueryPlan) -> Result<MutableQueryPlan, SQLError> {
        let control = &self.config.production;
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        for index in 0..block.group_by.len() {
            query = query.map_scalar(QueryScalarSlot::GroupBy(index), control, |expression| {
                self.scalar(expression)
            })?;
        }
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        for set in 0..block.grouping_sets.len() {
            let RelationalPlan::QueryBlock(block) = &query.root else {
                unreachable!()
            };
            for expression in 0..block.grouping_sets[set].len() {
                query = query.map_scalar(
                    QueryScalarSlot::GroupingSet { set, expression },
                    control,
                    |expression| self.scalar(expression),
                )?;
            }
        }
        Ok(query)
    }

    fn windows(&self, mut query: MutableQueryPlan) -> Result<MutableQueryPlan, SQLError> {
        let control = &self.config.production;
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        for window in 0..block.windows.len() {
            let RelationalPlan::QueryBlock(block) = &query.root else {
                unreachable!()
            };
            let (partitions, orders) = (
                block.windows[window].spec.partition_by.len(),
                block.windows[window].spec.order_by.len(),
            );
            for expression in 0..partitions {
                query = query.map_scalar(
                    QueryScalarSlot::WindowPartition { window, expression },
                    control,
                    |expression| self.scalar(expression),
                )?;
            }
            for expression in 0..orders {
                query = query.map_scalar(
                    QueryScalarSlot::WindowOrder { window, expression },
                    control,
                    |expression| self.scalar(expression),
                )?;
            }
            query = query.map_scalar(
                QueryScalarSlot::WindowFrameStart(window),
                control,
                |expression| self.scalar(expression),
            )?;
            query = query.map_scalar(
                QueryScalarSlot::WindowFrameEnd(window),
                control,
                |expression| self.scalar(expression),
            )?;
        }
        Ok(query)
    }
}
