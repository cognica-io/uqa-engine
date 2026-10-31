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
        self.compute_observing(&mut |expression| {
            Ok::<_, std::convert::Infallible>(crate::semantics::aggregates::contains_aggregate(
                aggregates, expression,
            ))
        })
        .unwrap_or_else(|never| match never {})
    }

    pub fn compute_kind_with_control(
        &self,
        aggregates: &dyn AggregateClassifier,
        control: &ProductionControl<'_>,
    ) -> Result<ComputePlan, SQLError> {
        control.check()?;
        self.compute_observing(&mut |expression| {
            crate::semantics::aggregates::contains_aggregate_with_control(
                aggregates, expression, control,
            )
        })
    }

    fn compute_observing<E>(
        &self,
        observe: &mut impl FnMut(&ScalarExpr) -> Result<bool, E>,
    ) -> Result<ComputePlan, E> {
        let mut aggregate =
            !self.group_by.is_empty() || !self.grouping_sets.is_empty() || self.having.is_some();
        if !aggregate {
            for expression in self
                .projections
                .iter()
                .map(|projection| &projection.expr)
                .chain(
                    self.windows
                        .iter()
                        .flat_map(|window| window.spec.expressions()),
                )
            {
                if observe(expression)? {
                    aggregate = true;
                    break;
                }
            }
        }
        let window = self
            .projections
            .iter()
            .any(|projection| projection.expr.contains_window());
        Ok(if aggregate {
            ComputePlan::Aggregate
        } else if window {
            ComputePlan::Window
        } else {
            ComputePlan::Project
        })
    }
}
