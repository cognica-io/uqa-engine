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
