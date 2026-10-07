//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL-plan contract for a caller-selected executable planner.

use super::UnifiedPlan;
use crate::{binding::statements::AnalyzedResult, SQLError, SQLParam};

/// Analyze and optimize a logical plan using metadata captured for this call.
pub trait ExecutablePlanOptimizer {
    /// Reuse analysis only after the caller validates its catalog/scope in the selected snapshot. Optimization still runs for this invocation. The default retains full analysis for implementations without this capability.
    fn plan_with_cached_analysis(
        &self,
        plan: UnifiedPlan,
        params: &[SQLParam],
        _cached: Option<std::sync::Arc<crate::binding::statements::AnalyzedStatement>>,
    ) -> Result<
        (
            UnifiedPlan,
            Option<std::sync::Arc<crate::binding::statements::AnalyzedStatement>>,
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
    ) -> Result<(UnifiedPlan, bool), SQLError> {
        self.plan_for_execution(plan, params)
            .map(|plan| (plan, false))
    }

    fn plan_for_execution(
        &self,
        plan: UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<UnifiedPlan, SQLError>;

    /// Analyze and optimize a logical plan, and return what its analysis derives about its result.
    fn plan_with_result(
        &self,
        plan: UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<(UnifiedPlan, AnalyzedResult), SQLError>;
}
