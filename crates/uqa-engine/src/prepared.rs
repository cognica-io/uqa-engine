//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::Engine;

impl Engine {
    pub fn register_prepared(
        &self,
        name: String,
        definition: uqa_sql::ast::Statement,
    ) -> Result<(), uqa_sql::SQLError> {
        self.with_direct_read_snapshot(|engine| {
            uqa_execution::statement::prepared::register_statement(
                &engine.prepared_registration_context(),
                name,
                definition,
            )
        })
    }

    pub(crate) fn prepared_parameter_types(
        &self,
        name: &str,
    ) -> Option<Vec<Option<uqa_sql::ast::ColumnType>>> {
        self.session
            .prepared
            .read()
            .get(name)
            .map(|entry| entry.parameter_types.clone())
    }

    pub fn lookup_prepared(&self, name: &str) -> Option<uqa_planner::UnifiedPlan> {
        self.session
            .prepared
            .read()
            .get(name)
            .map(|entry| entry.plan.as_ref().unwrap_or(&entry.logical_plan).clone())
    }

    /// Invalidate executable plans without removing connection-owned definitions.
    /// Catalog changes are checked when each statement is next executed, so a
    /// dropped dependency cannot make unrelated commands or rollback fail.
    pub(crate) fn invalidate_prepared_plans(&self) {
        uqa_execution::statement::prepared::invalidation::invalidate_execution_plans(
            self.session.prepared.write().values_mut(),
        );
    }

    /// Read original input syntax again after a binding dependency changes.
    pub(crate) fn invalidate_prepared_analysis(&self) {
        uqa_execution::statement::prepared::invalidation::PreparedCatalogChange::GlobalCatalog
            .invalidate_with_routines(
                self.session.prepared.write().values_mut(),
                &self.session.routine_bodies,
            );
    }

    pub fn deallocate_prepared(&self, name: Option<&str>) {
        match name {
            Some(name) => {
                self.session.prepared.write().remove(name);
            }
            None => self.session.prepared.write().clear(),
        }
    }
}
