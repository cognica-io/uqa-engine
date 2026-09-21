//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Connect state-owned readers to the catalog executor's narrow services.

use super::SessionExecutionView;
use crate::Engine;
use uqa_execution::catalog::{
    context::CatalogContext,
    services::{CatalogNamespace, CatalogSession, RelationCounts},
    RelationNameResolution,
};
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::catalog::session::{CursorMetadata, PreparedStatementMetadata};
use uqa_sql::SQLError;

impl Engine {
    pub(crate) fn catalog_identity_reservation_context(
        &self,
    ) -> uqa_execution::catalog::identity::CatalogIdentityReservationContext<'_> {
        uqa_execution::catalog::identity::CatalogIdentityReservationContext {
            catalog: self,
            session: self,
            locks: self,
        }
    }

    pub(crate) fn catalog_execution(&self) -> CatalogContext<'_> {
        CatalogContext {
            catalog: self,
            session: self,
            namespaces: self,
            routines: self,
            expressions: self,
            counts: self,
            views: self,
            cache: &self.runtime.regtype_output_cache,
        }
    }
}
impl CatalogSession for Engine {
    fn current_role(&self) -> RoleReference {
        self.session_execution_view().current_role()
    }
    fn temporary_schema_name(&self) -> String {
        self.session_execution_view().temporary_schema_name()
    }
    fn relation_name_resolution(&self) -> RelationNameResolution {
        self.session_execution_view().relation_name_resolution()
    }

    fn show_variable(&self, name: &str) -> Result<String, SQLError> {
        self.session_execution_view().show_variable(name)
    }
    fn runtime_parameter_source(&self, name: &str) -> &'static str {
        self.session_execution_view().runtime_parameter_source(name)
    }
    fn prepared_statements(&self) -> Vec<PreparedStatementMetadata> {
        self.session_execution_view().prepared_statements()
    }
    fn cursors(&self) -> Vec<CursorMetadata> {
        self.session_execution_view().cursors()
    }
}
impl CatalogSession for SessionExecutionView<'_> {
    fn current_role(&self) -> RoleReference {
        SessionExecutionView::current_role(self)
    }
    fn temporary_schema_name(&self) -> String {
        SessionExecutionView::temporary_schema_name(self)
    }
    fn relation_name_resolution(&self) -> RelationNameResolution {
        SessionExecutionView::relation_name_resolution(self)
    }

    fn show_variable(&self, name: &str) -> Result<String, SQLError> {
        SessionExecutionView::show_variable(self, name)
    }
    fn runtime_parameter_source(&self, name: &str) -> &'static str {
        SessionExecutionView::runtime_parameter_source(self, name)
    }
    fn prepared_statements(&self) -> Vec<PreparedStatementMetadata> {
        SessionExecutionView::prepared_statements(self)
    }
    fn cursors(&self) -> Vec<CursorMetadata> {
        SessionExecutionView::cursors(self)
    }
}
impl RelationCounts for Engine {
    fn table_doc_count(&self, name: &str) -> Result<u64, SQLError> {
        Engine::table_doc_count(self, name)
    }
}
impl CatalogNamespace for Engine {
    fn current_schema_names(&self, implicit: bool) -> Result<Vec<String>, SQLError> {
        Engine::current_schema_names(self, implicit)
            .map_err(|error| SQLError::Internal(error.to_string()))
    }
    fn current_schema_names_with_catalog(
        &self,
        catalog: &uqa_execution::catalog::CatalogReadView,
        implicit: bool,
    ) -> Result<Vec<String>, SQLError> {
        Ok(uqa_execution::catalog::namespaces::current_schema_names(
            catalog,
            &self.session_execution_view().relation_name_resolution(),
            &self.current_role(),
            implicit,
        ))
    }
}

impl uqa_execution::catalog::services::CatalogSnapshotSource for Engine {
    fn bind_query_reads(
        &self,
        snapshot: uqa_execution::catalog::CatalogReadView,
    ) -> Result<uqa_execution::catalog::CatalogReadView, SQLError> {
        let map_error =
            |error| uqa_execution::storage_errors::storage_error("bind catalog reads", &error);
        let Some(context) = self.graph_read_context().map_err(map_error)? else {
            return Ok(snapshot);
        };
        Ok(snapshot.with_graph_reads(
            self.new_graph_store().map_err(map_error)?,
            context,
            &self.runtime.cancellation,
        ))
    }

    fn refreshed_catalog_snapshot(
        &self,
    ) -> Result<uqa_execution::catalog::CatalogReadView, SQLError> {
        self.synchronize_table_catalog()
            .map_err(|error| SQLError::Internal(format!("load table catalog: {error}")))?;
        self.synchronize_catalog_registries()
            .map_err(|error| SQLError::Internal(format!("load relation catalog: {error}")))?;
        Ok(self.catalog_read_view())
    }

    fn catalog_snapshot(&self) -> uqa_execution::catalog::CatalogReadView {
        self.catalog_read_view()
    }

    fn current_catalog_snapshot(&self) -> uqa_execution::catalog::CatalogReadView {
        self.restored_catalog_read_view()
    }
}

use uqa_core::Value;
impl Engine {
    pub(crate) fn clear_regtype_output_cache(&self) {
        self.runtime.regtype_output_cache.clear();
    }
}

use uqa_execution::catalog::services::{
    CatalogExpressionEvaluation, ViewCatalogCapabilities, ViewCatalogMetadata,
};
use uqa_sql::ast::{Expr, TriggerEvent};
impl CatalogExpressionEvaluation for Engine {
    fn evaluate(&self, expression: &Expr) -> Result<Value, SQLError> {
        crate::capabilities::query_expressions::eval_lowered_expression(self, expression, None, &[])
    }
}
impl ViewCatalogCapabilities for Engine {
    fn view_updatability(&self, name: &str) -> Result<ViewCatalogMetadata, SQLError> {
        let metadata =
            uqa_sql::semantics::view_rewrite::view_updatability(self.view_rewrite_context(), name)?;
        Ok(ViewCatalogMetadata {
            catalog: metadata.catalog,
            catalog_columns: metadata.catalog_columns,
            check_option: metadata.check_option,
        })
    }
    fn has_instead_of_trigger(&self, name: &str, event: TriggerEvent) -> Result<bool, SQLError> {
        uqa_sql::semantics::view_rewrite::has_instead_of_trigger(
            self.view_rewrite_context(),
            name,
            event,
        )
    }
}
