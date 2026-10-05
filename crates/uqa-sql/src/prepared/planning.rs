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

/// A successful replacement analysis and the namespace it used, published together against the original definition identity.
pub struct PreparedPlanAnalysis {
    pub logical_plan: std::sync::Arc<UnifiedPlan>,
    pub effective_search_path: Option<crate::catalog::resolution::EffectiveSearchPath>,
    pub dependencies: super::dependencies::PreparedAnalysisDependencies,
    pub dependency_snapshot: Option<super::dependencies::PreparedDependencySnapshot>,
}

pub struct PreparedPlanUpdate {
    pub reanalyzed: Option<PreparedPlanAnalysis>,
    pub generic_plan: Option<UnifiedPlan>,
    pub generic_cost: Option<f64>,
    pub custom_cost: Option<f64>,
}
