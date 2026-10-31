//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Register execution-owned extension contracts in the engine.

pub use uqa_execution::functions::*;

impl uqa_sql::plan::AggregateClassifier for crate::Engine {
    fn is_registered_aggregate(&self, name: &str) -> bool {
        self.has_registered_aggregate_function(name)
    }

    fn is_registered_aggregate_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, uqa_sql::SQLError> {
        self.query_runtime_view()
            .has_aggregate_function_with_control(name, control)
    }
}

impl uqa_execution::functions::AggregateFunctionRegistry for crate::Engine {
    fn registered_aggregate_function(
        &self,
        name: &str,
    ) -> Option<std::sync::Arc<dyn SQLAggregateFactory>> {
        self.registered_aggregate_function(name)
    }
}
