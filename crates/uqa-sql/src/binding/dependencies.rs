//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preparation dependencies attached to successful catalog binding after CTE resolution.

use super::{SQLError, SchemaScope};

impl SchemaScope {
    pub(super) fn record_relation_dependency(&mut self, name: &str) -> Result<(), SQLError> {
        let Some(dependencies) = &mut self.prepared_dependencies else {
            return Ok(());
        };
        if let Some(oid) = self.catalog.relation_dependency(&self.resolution, name)? {
            dependencies.relations.insert(oid);
        }
        Ok(())
    }

    pub(super) fn record_routine_dependency(&mut self, binding: &crate::ast::FunctionBinding) {
        if let Some(dependencies) = &mut self.prepared_dependencies {
            dependencies.include_routine(binding);
        }
    }

    pub(super) fn record_stored_query_dependencies(&mut self, query: &crate::plan::QueryPlan) {
        if let Some(dependencies) = &mut self.prepared_dependencies {
            query.visit_scalar_expressions(&mut |expression| {
                dependencies.include_expression(expression);
            });
        }
    }
}
