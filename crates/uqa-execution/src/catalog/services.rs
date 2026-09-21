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
    fn show_variable(&self, name: &str) -> Result<String, SQLError>;
    fn runtime_parameter_source(&self, name: &str) -> &'static str;
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
