//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-only test inputs reject accidental row reads and expression execution.

use crate::catalog::{
    cache::RegtypeOutputCache,
    context::CatalogContext,
    services::{
        CatalogExpressionEvaluation, CatalogNamespace, CatalogSession, RelationCounts,
        ViewCatalogCapabilities, ViewCatalogMetadata,
    },
    CatalogReadView, RelationNameResolution,
};
use uqa_core::Value;
use uqa_sql::{
    ast::{Expr, TriggerEvent},
    SQLError,
};

pub(crate) struct CatalogServices {
    pub resolution: RelationNameResolution,
    pub standard_conforming_strings: Option<bool>,
}

impl Default for CatalogServices {
    fn default() -> Self {
        Self {
            standard_conforming_strings: None,
            resolution: RelationNameResolution {
                search_path: vec!["public".into()],
                temporary_schema: "pg_temp_1".into(),
                temporary_namespace_allocated: false,
                current_user: "uqa".into(),
                lookup_mode: crate::catalog::RelationLookupMode::Bound,
            },
        }
    }
}

impl CatalogServices {
    pub fn context<'a>(
        &'a self,
        catalog: &'a CatalogReadView,
        cache: &'a RegtypeOutputCache,
    ) -> CatalogContext<'a> {
        CatalogContext {
            catalog,
            session: self,
            namespaces: self,
            routines: &super::NoRoutines,
            expressions: self,
            counts: self,
            views: self,
            cache,
        }
    }
}

impl CatalogSession for CatalogServices {
    fn current_role(&self) -> uqa_sql::catalog::roles::RoleReference {
        self.resolution.current_user.clone()
    }
    fn temporary_schema_name(&self) -> String {
        self.resolution.temporary_schema.clone()
    }
    fn relation_name_resolution(&self) -> RelationNameResolution {
        self.resolution.clone()
    }
    fn show_parameter(&self, name: &str) -> Result<(String, String), SQLError> {
        if let ("standard_conforming_strings", Some(enabled)) =
            (name, self.standard_conforming_strings)
        {
            return Ok((name.into(), if enabled { "on" } else { "off" }.into()));
        }
        Err(SQLError::Internal("unexpected parameter read".into()))
    }
    fn parameter_settings(&self) -> Vec<uqa_sql::semantics::parameters::setting::ParameterSetting> {
        Vec::new()
    }
    fn prepared_statements(&self) -> Vec<uqa_sql::catalog::session::PreparedStatementMetadata> {
        Vec::new()
    }
    fn cursors(&self) -> Vec<uqa_sql::catalog::session::CursorMetadata> {
        Vec::new()
    }
}

impl CatalogNamespace for CatalogServices {
    fn current_schema_names(&self, _: bool) -> Result<Vec<String>, SQLError> {
        Err(SQLError::Internal("catalog selection is required".into()))
    }
    fn current_schema_names_with_catalog(
        &self,
        catalog: &CatalogReadView,
        implicit: bool,
    ) -> Result<Vec<String>, SQLError> {
        Ok(crate::catalog::namespaces::current_schema_names(
            catalog,
            &self.resolution,
            &self.resolution.current_user,
            implicit,
        ))
    }
}

impl CatalogExpressionEvaluation for CatalogServices {
    fn evaluate(&self, _: &Expr) -> Result<Value, SQLError> {
        Err(SQLError::Internal("unexpected expression execution".into()))
    }
}

impl RelationCounts for CatalogServices {
    fn table_doc_count(&self, _: &str) -> Result<u64, SQLError> {
        Err(SQLError::Internal("unexpected table read".into()))
    }
}

impl ViewCatalogCapabilities for CatalogServices {
    fn view_updatability(&self, _: &str) -> Result<ViewCatalogMetadata, SQLError> {
        Err(SQLError::Internal(
            "unexpected view mutation analysis".into(),
        ))
    }
    fn has_instead_of_trigger(&self, _: &str, _: TriggerEvent) -> Result<bool, SQLError> {
        Err(SQLError::Internal("unexpected view trigger read".into()))
    }
}
