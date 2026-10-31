//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Executable variants owned by one successful routine statement analysis.

use parking_lot::Mutex;
use std::sync::Arc;
use uqa_sql::{
    prepared::{
        definition::PreparedDefinition, entry::PreparedStatementPlan,
        planning::PreparedPlanSelection,
    },
    SQLError,
};

pub(super) struct RoutinePlanVariants(Mutex<Variants>);

struct Variants {
    entry: PreparedStatementPlan,
    generation: Arc<()>,
}

impl RoutinePlanVariants {
    pub(super) fn new(
        definition: &PreparedDefinition<uqa_sql::plan::SharedExecutablePlan>,
    ) -> Self {
        let logical_plan = definition.logical_plan.clone();
        Self(Mutex::new(Variants {
            generation: Arc::new(()),
            entry: PreparedStatementPlan {
                definition_identity: Arc::new(()),
                composite_inputs: definition.composite_inputs.clone(),
                source_plan: logical_plan.clone(),
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
            },
        }))
    }

    pub(super) fn select(
        &self,
        select: impl FnOnce(&PreparedStatementPlan) -> Result<PreparedPlanSelection, SQLError>,
    ) -> Result<uqa_sql::plan::SharedExecutablePlan, SQLError> {
        let (entry, generation) = {
            let current = self.0.lock();
            (current.entry.clone(), Arc::clone(&current.generation))
        };
        let selected = select(&entry)?;
        let mut current = self.0.lock();
        if Arc::ptr_eq(&current.generation, &generation)
            && current.entry.logical_plan.ptr_eq(&entry.logical_plan)
        {
            current.entry.record_execution(selected.update);
        }
        Ok(selected.plan)
    }

    pub(super) fn invalidate(&self) {
        let mut current = self.0.lock();
        if !current.entry.has_tracked_executable_dependencies() {
            current.entry.plan = None;
            // Reentrant catalog publication retires only this selection generation. The analyzed input remains immutable and does not need a new allocation identity.
            current.generation = Arc::new(());
        }
    }

    #[cfg(test)]
    pub(super) fn snapshot(&self) -> PreparedStatementPlan {
        self.0.lock().entry.clone()
    }
}

#[cfg(test)]
mod tests;
