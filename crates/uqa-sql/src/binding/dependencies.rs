//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preparation dependencies attached to successful catalog binding after CTE resolution.

use super::{SQLError, SchemaScope};

impl SchemaScope {
    pub(super) fn record_relation_dependency(&mut self, name: &str) -> Result<(), SQLError> {
        let owner = self.control().capture();
        let control = owner.control();
        let Some(dependencies) = &mut self.prepared_dependencies else {
            return Ok(());
        };
        if let Some(oid) =
            self.catalog
                .relation_dependency_with_control(&self.resolution, name, &control)?
        {
            dependencies.include_relation_with_control(oid, &control)?;
        }
        Ok(())
    }

    pub(super) fn record_routine_dependency(
        &mut self,
        binding: &crate::ast::FunctionBinding,
    ) -> Result<(), SQLError> {
        let owner = self.control().capture();
        if let Some(dependencies) = &mut self.prepared_dependencies {
            dependencies.include_routine_with_control(binding, &owner.control())?;
        }
        Ok(())
    }

    pub(super) fn record_stored_query_dependencies(
        &mut self,
        query: &crate::plan::QueryPlan,
    ) -> Result<(), SQLError> {
        let owner = self.control().capture();
        if let Some(dependencies) = &mut self.prepared_dependencies {
            let mut result = Ok(());
            query.visit_scalar_expressions(&mut |expression| {
                if result.is_ok() {
                    result =
                        dependencies.include_expression_with_control(expression, &owner.control());
                }
            });
            result?;
        }
        Ok(())
    }
}
