//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consumer-owned services required to project catalog rows.
use super::RelationNameResolution;
use uqa_core::Value;
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::catalog::session::{CursorMetadata, PreparedStatementMetadata};
use uqa_sql::{
    ast::{Expr, TriggerEvent},
    SQLError,
};

pub trait CatalogSession: Sync {
    fn current_role(&self) -> RoleReference;
    fn temporary_schema_name(&self) -> String;
    fn relation_name_resolution(&self) -> RelationNameResolution;
    /// The canonical name of the parameter `name` refers to and its value as `SHOW` reports it.
    fn show_parameter(&self, name: &str) -> Result<(String, String), SQLError>;
    /// Every parameter that `SHOW ALL` and `pg_settings` report, in name order.
    fn parameter_settings(&self) -> Vec<uqa_sql::semantics::parameters::setting::ParameterSetting>;
    fn prepared_statements(&self) -> Vec<PreparedStatementMetadata>;
    fn cursors(&self) -> Vec<CursorMetadata>;
}
pub trait CatalogExpressionEvaluation: Sync {
    fn evaluate(&self, expression: &Expr) -> Result<Value, SQLError>;
}
pub trait RelationCounts: Sync {
    fn table_doc_count(&self, table: &str) -> Result<u64, SQLError>;
}
pub use uqa_sql::catalog::view::ViewMutationCapabilities;
pub struct ViewCatalogMetadata {
    pub catalog: ViewMutationCapabilities,
    pub catalog_columns: Vec<bool>,
    pub check_option: String,
}
pub trait ViewCatalogCapabilities: Sync {
    fn view_updatability(&self, name: &str) -> Result<ViewCatalogMetadata, SQLError>;
    fn has_instead_of_trigger(&self, name: &str, event: TriggerEvent) -> Result<bool, SQLError>;
    fn view_updatability_with_catalog(
        &self,
        name: &str,
        _catalog: &super::CatalogReadView,
        _resolution: &RelationNameResolution,
    ) -> Result<ViewCatalogMetadata, SQLError> {
        self.view_updatability(name)
    }
    fn has_instead_of_trigger_with_catalog(
        &self,
        name: &str,
        event: TriggerEvent,
        _catalog: &super::CatalogReadView,
        _resolution: &RelationNameResolution,
    ) -> Result<bool, SQLError> {
        self.has_instead_of_trigger(name, event)
    }
}

pub trait CatalogNamespace: Sync {
    fn current_schema_names(&self, include_implicit: bool) -> Result<Vec<String>, SQLError>;
    /// Resolve visibility through a caller-selected catalog while preserving legacy namespace services.
    fn current_schema_names_with_catalog(
        &self,
        _catalog: &super::CatalogReadView,
        include_implicit: bool,
    ) -> Result<Vec<String>, SQLError> {
        self.current_schema_names(include_implicit)
    }
}

pub trait CatalogSnapshotSource: Sync {
    fn catalog_snapshot(&self) -> super::CatalogReadView;
    fn refreshed_catalog_snapshot(&self) -> Result<super::CatalogReadView, SQLError>;
    /// Refresh definitions without capturing a query view that definition coordination will discard.
    fn refresh_catalog(&self) -> Result<(), SQLError> {
        self.refreshed_catalog_snapshot().map(|_| ())
    }
    /// Attach the original query participant without replacing the retained catalog or observing unused sources.
    fn bind_query_reads(
        &self,
        snapshot: super::CatalogReadView,
    ) -> Result<super::CatalogReadView, SQLError> {
        Ok(snapshot)
    }
    /// Definition coordination uses the refreshed session catalog, independently from an ordinary query's retained snapshot.
    fn current_catalog_snapshot(&self) -> super::CatalogReadView;
}
impl CatalogSnapshotSource for super::CatalogReadView {
    fn refresh_catalog(&self) -> Result<(), SQLError> {
        Ok(())
    }
    fn current_catalog_snapshot(&self) -> super::CatalogReadView {
        self.clone()
    }
    fn refreshed_catalog_snapshot(&self) -> Result<super::CatalogReadView, SQLError> {
        Ok(self.clone())
    }
    fn catalog_snapshot(&self) -> super::CatalogReadView {
        self.clone()
    }
}
