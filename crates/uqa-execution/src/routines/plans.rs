//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Executable variants owned by one successful routine statement analysis.

use parking_lot::Mutex;
use std::sync::Arc;
use uqa_sql::{
    plan::UnifiedPlan,
    prepared::{
        definition::PreparedDefinition, entry::PreparedStatementPlan,
        planning::PreparedPlanSelection,
    },
    SQLError,
};

pub(super) struct RoutinePlanVariants(Mutex<PreparedStatementPlan>);

impl RoutinePlanVariants {
    pub(super) fn new(definition: &PreparedDefinition) -> Self {
        let logical_plan = Arc::new(definition.logical_plan.clone());
        Self(Mutex::new(PreparedStatementPlan {
            source_plan: Arc::clone(&logical_plan),
            logical_plan,
            needs_analysis: false,
            effective_search_path: definition.effective_search_path.clone(),
            dependencies: definition.dependencies.clone(),
            dependency_snapshot: definition.dependency_snapshot.clone(),
            plan: None,
            parameter_types: definition.parameter_types.clone(),
            result_schema: definition.result_schema.clone(),
            source_sql: None,
            prepared_at_micros: 0,
            from_sql: false,
            generic_plans: 0,
            custom_plans: 0,
            generic_cost: None,
            total_custom_cost: 0.0,
        }))
    }

    pub(super) fn select(
        &self,
        select: impl FnOnce(&PreparedStatementPlan) -> Result<PreparedPlanSelection, SQLError>,
    ) -> Result<UnifiedPlan, SQLError> {
        let entry = self.0.lock().clone();
        let selected = select(&entry)?;
        let mut current = self.0.lock();
        if Arc::ptr_eq(&current.logical_plan, &entry.logical_plan) {
            current.record_execution(selected.update);
        }
        Ok(selected.plan)
    }

    pub(super) fn invalidate(&self) {
        let mut entry = self.0.lock();
        if !entry.has_tracked_executable_dependencies() {
            entry.plan = None;
            // Optimizer callbacks can publish catalog changes. A fresh identity
            // prevents an older callback from restoring a discarded executable.
            entry.logical_plan = Arc::new((*entry.logical_plan).clone());
        }
    }

    #[cfg(test)]
    pub(super) fn snapshot(&self) -> PreparedStatementPlan {
        self.0.lock().clone()
    }
}
