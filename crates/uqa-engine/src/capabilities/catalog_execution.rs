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
use uqa_sql::catalog::session::PreparedStatementMetadata;
use uqa_sql::SQLError;

impl Engine {
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
    fn current_user(&self) -> String {
        self.session_execution_view().current_user()
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
}
impl CatalogSession for SessionExecutionView<'_> {
    fn current_user(&self) -> String {
        SessionExecutionView::current_user(self)
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
}

impl uqa_execution::catalog::services::CatalogSnapshotSource for Engine {
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
}
