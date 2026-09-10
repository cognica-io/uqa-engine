//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply read-only function and view metadata to SQL semantic analysis.

use crate::Engine;
use uqa_sql::{
    ast::{FunctionBinding, FunctionVolatility},
    plan::QueryPlan,
    SQLError,
};

impl uqa_sql::semantics::volatility::VolatilityCatalog for Engine {
    fn host_function_volatility(&self, name: &str) -> Option<FunctionVolatility> {
        self.registered_runtime_function_volatility(name)
    }

    fn routine_volatilities(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>> {
        let overloads = match binding {
            Some(binding) if binding.builtin => None,
            Some(binding) => self.lookup_bound_sql_functions_by_binding(binding),
            None => self
                .lookup_visible_sql_functions_for_analysis(name)
                .ok()
                .flatten(),
        }?;
        Some(
            overloads
                .iter()
                .map(|function| function.def.volatility)
                .collect(),
        )
    }

    fn view_query(&self, name: &str) -> Result<Option<QueryPlan>, SQLError> {
        self.view_plan(name)
    }
}
