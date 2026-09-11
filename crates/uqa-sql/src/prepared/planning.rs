//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared-plan selection contract and identity-checked cache updates.

use crate::{plan::UnifiedPlan, SQLError, SQLParam};

pub trait PreparedPlanProvider {
    fn plan_for_execution(
        &self,
        name: &str,
        parameters: &[SQLParam],
    ) -> Result<Option<UnifiedPlan>, SQLError>;
}

pub struct PreparedPlanUpdate {
    pub generic_plan: Option<UnifiedPlan>,
    pub generic_cost: Option<f64>,
    pub custom_cost: Option<f64>,
}
