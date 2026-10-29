//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared-plan selection contract and identity-checked cache updates.

use super::ownership::SharedPreparedInput;
use crate::{SQLError, SQLParam};

pub trait PreparedPlanProvider {
    fn plan_for_execution(
        &self,
        name: &str,
        parameters: &[SQLParam],
    ) -> Result<Option<crate::plan::SharedExecutablePlan>, SQLError>;
}

/// A successful replacement analysis and the namespace it used, published together against the original definition identity.
pub struct PreparedPlanAnalysis {
    pub logical_plan: crate::plan::SharedExecutablePlan,
    pub effective_search_path:
        SharedPreparedInput<Option<crate::catalog::resolution::EffectiveSearchPath>>,
    pub dependencies: SharedPreparedInput<super::dependencies::PreparedAnalysisDependencies>,
    pub dependency_snapshot:
        SharedPreparedInput<Option<super::dependencies::PreparedDependencySnapshot>>,
}

pub struct PreparedPlanUpdate {
    pub composite_inputs: super::composites::CompositeInputs,
    pub reanalyzed: Option<PreparedPlanAnalysis>,
    pub generic_plan: Option<crate::plan::SharedExecutablePlan>,
    pub generic_cost: Option<f64>,
    pub custom_cost: Option<f64>,
}

/// A selected immutable executable and its cache update. Reuse shares the cached executable; custom specialization and composite projection retain separate plans. The owner publishes the update only against the definition it supplied to selection.
pub struct PreparedPlanSelection {
    pub plan: crate::plan::SharedExecutablePlan,
    pub update: PreparedPlanUpdate,
}
