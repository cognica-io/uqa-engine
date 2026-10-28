//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared-plan selection contract and identity-checked cache updates.

use crate::{plan::UnifiedPlan, SQLError, SQLParam};
use std::sync::Arc;

pub trait PreparedPlanProvider {
    fn plan_for_execution(
        &self,
        name: &str,
        parameters: &[SQLParam],
    ) -> Result<Option<Arc<UnifiedPlan>>, SQLError>;
}

/// A successful replacement analysis and the namespace it used, published together against the original definition identity.
pub struct PreparedPlanAnalysis {
    pub logical_plan: Arc<UnifiedPlan>,
    pub effective_search_path: Option<crate::catalog::resolution::EffectiveSearchPath>,
    pub dependencies: super::dependencies::PreparedAnalysisDependencies,
    pub dependency_snapshot: Option<super::dependencies::PreparedDependencySnapshot>,
}

pub struct PreparedPlanUpdate {
    pub composite_inputs: super::composites::CompositeInputs,
    pub reanalyzed: Option<PreparedPlanAnalysis>,
    pub generic_plan: Option<Arc<UnifiedPlan>>,
    pub generic_cost: Option<f64>,
    pub custom_cost: Option<f64>,
}

/// A selected immutable executable and its cache update. Reuse shares the cached executable; custom specialization and composite projection retain separate plans. The owner publishes the update only against the definition it supplied to selection.
pub struct PreparedPlanSelection {
    pub plan: Arc<UnifiedPlan>,
    pub update: PreparedPlanUpdate,
}
