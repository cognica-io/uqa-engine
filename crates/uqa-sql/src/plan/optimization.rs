//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL-plan contract for a caller-selected executable planner.

use super::{SharedExecutablePlan, UnifiedPlan};
use crate::{binding::statements::AnalyzedResult, SQLError, SQLParam};
use uqa_core::memory::{OwnedProductionControl, ProductionControl};

/// Analyze and optimize a logical plan using metadata captured for this call.
pub trait ExecutablePlanOptimizer {
    fn planning_control(&self) -> Result<OwnedProductionControl, SQLError> {
        Ok(ProductionControl::uncontrolled().capture())
    }

    fn plan_owned_for_execution(
        &self,
        plan: super::MutableExecutablePlan,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<SharedExecutablePlan, SQLError> {
        self.plan_for_execution(ordinary_plan(plan, control)?, params)
    }

    fn plan_owned_with_result(
        &self,
        plan: super::MutableExecutablePlan,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<(SharedExecutablePlan, AnalyzedResult), SQLError> {
        self.plan_with_result(ordinary_plan(plan, control)?, params)
    }

    fn plan_owned_for_statement_cache(
        &self,
        plan: super::MutableExecutablePlan,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<(SharedExecutablePlan, bool), SQLError> {
        self.plan_for_statement_cache(ordinary_plan(plan, control)?, params)
    }

    fn plan_owned_with_cached_analysis(
        &self,
        plan: super::MutableExecutablePlan,
        params: &[SQLParam],
        cached: Option<crate::binding::statements::AnalyzedStatement>,
        control: &ProductionControl<'_>,
    ) -> Result<
        (
            SharedExecutablePlan,
            Option<crate::binding::statements::AnalyzedStatement>,
        ),
        SQLError,
    > {
        self.plan_with_cached_analysis(ordinary_plan(plan, control)?, params, cached)
    }
    /// Reuse analysis only after the caller validates its catalog/scope in the selected snapshot. Optimization still runs for this invocation. The default retains full analysis for implementations without this capability.
    fn plan_with_cached_analysis(
        &self,
        plan: UnifiedPlan,
        params: &[SQLParam],
        _cached: Option<crate::binding::statements::AnalyzedStatement>,
    ) -> Result<
        (
            SharedExecutablePlan,
            Option<crate::binding::statements::AnalyzedStatement>,
        ),
        SQLError,
    > {
        self.plan_for_execution(plan, params)
            .map(|plan| (plan, None))
    }

    /// Return an executable and whether its analysis inputs permit reuse across
    /// ordinary messages. Planners without that evidence must analyze again.
    fn plan_for_statement_cache(
        &self,
        plan: UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<(SharedExecutablePlan, bool), SQLError> {
        self.plan_for_execution(plan, params)
            .map(|plan| (plan, false))
    }

    fn plan_for_execution(
        &self,
        plan: UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<SharedExecutablePlan, SQLError>;

    /// Analyze and optimize a logical plan, and return what its analysis derives about its result.
    fn plan_with_result(
        &self,
        plan: UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<(SharedExecutablePlan, AnalyzedResult), SQLError>;
}

/// Legacy adapters run only with their declared ordinary owner. Controlled planners must implement the consuming entry points and retain every lease in their returned executable.
fn ordinary_plan(
    plan: super::MutableExecutablePlan,
    control: &ProductionControl<'_>,
) -> Result<UnifiedPlan, SQLError> {
    if control.budget().is_some() {
        return Err(SQLError::Unsupported(
            "executable planner does not implement controlled plan ownership".into(),
        ));
    }
    Ok(plan.finish(control)?.into_parts().0)
}
