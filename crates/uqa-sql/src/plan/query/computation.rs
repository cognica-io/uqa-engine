//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Computation selection shares ordered aggregate observation across ordinary and admitted callers.

use super::{AggregateClassifier, ComputePlan, QueryBlockPlan, ScalarExpr};
use crate::SQLError;
use uqa_core::memory::ProductionControl;

impl QueryBlockPlan {
    /// Select the computation before output-demand rewrites remove unobserved aggregate values.
    pub fn compute_kind(&self, aggregates: &dyn AggregateClassifier) -> ComputePlan {
        crate::catalog::work::inline(self.compute_with_work(
            &ProductionControl::uncontrolled(),
            &mut (),
            &mut |expression| {
                Ok(crate::semantics::aggregates::is_aggregate(
                    aggregates, expression,
                ))
            },
        ))
        .expect("ordinary computation observation")
    }

    pub fn compute_kind_with_control(
        &self,
        aggregates: &dyn AggregateClassifier,
        control: &ProductionControl<'_>,
    ) -> Result<ComputePlan, SQLError> {
        crate::catalog::work::inline(self.compute_with_work(control, &mut (), &mut |expression| {
            crate::semantics::aggregates::is_aggregate_with_control(aggregates, expression, control)
        }))
    }

    /// Retain aggregate and window discovery within the selected work window.
    pub async fn compute_kind_from_input(
        &self,
        aggregates: &(dyn AggregateClassifier + Sync),
        control: &ProductionControl<'_>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> Result<ComputePlan, SQLError> {
        self.compute_with_work(control, input, &mut |expression| {
            crate::semantics::aggregates::is_aggregate_with_control(aggregates, expression, control)
        })
        .await
    }

    pub(in crate::plan) async fn compute_with_work<W, O>(
        &self,
        control: &ProductionControl<'_>,
        work: &mut W,
        classify: &mut O,
    ) -> Result<ComputePlan, SQLError>
    where
        W: crate::catalog::work::Work,
        O: FnMut(&ScalarExpr) -> Result<bool, SQLError>,
    {
        control.check()?;
        if !self.group_by.is_empty() || !self.grouping_sets.is_empty() || self.having.is_some() {
            return Ok(ComputePlan::Aggregate);
        }
        for projection in &self.projections {
            if crate::semantics::aggregates::contains_with_work(
                &projection.expr,
                control,
                work,
                classify,
            )
            .await?
            {
                return Ok(ComputePlan::Aggregate);
            }
        }
        for window in &self.windows {
            // An empty definition still consumes work; flat_map would silently drain an unbounded empty prefix.
            work.checkpoint().await?;
            control.check()?;
            for expression in window.spec.expressions() {
                if crate::semantics::aggregates::contains_with_work(
                    expression, control, work, classify,
                )
                .await?
                {
                    return Ok(ComputePlan::Aggregate);
                }
            }
        }
        for projection in &self.projections {
            if projection
                .expr
                .contains_window_with_work(control, work)
                .await?
            {
                return Ok(ComputePlan::Window);
            }
        }
        Ok(ComputePlan::Project)
    }
}

#[cfg(test)]
mod tests;
