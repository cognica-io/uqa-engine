//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::security::BoundTableSecurity;
use std::ops::{Deref, DerefMut};

/// One bound view query together with the fixed public column names captured when the view was created. `None` exists only while the catalog-opening migration reads formats written before column metadata was persisted.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoredView {
    /// Security is loaded independently from the durable catalog row.
    #[serde(skip)]
    pub security: BoundTableSecurity,
    #[serde(flatten)]
    pub definition: StoredViewDefinition,
}

/// Query-definition JSON carries no authorization state. Restoration supplies validated security before publishing a view.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredViewDefinition {
    /// Stable logical relation identity. Renames and replacement preserve it; a zero value marks a legacy catalog row upgraded during initial open.
    #[serde(default)]
    pub object_id: [u8; 16],
    pub query: crate::plan::QueryPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_columns: Option<Vec<String>>,
    #[serde(default)]
    pub persistence: crate::ast::RelationPersistence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<(String, String)>,
    #[serde(default)]
    pub kind: StoredViewKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub materialized_rows: Vec<crate::ResultRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub materialized_column_types: Vec<Option<crate::ast::ColumnType>>,
    #[serde(default = "default_view_populated")]
    pub populated: bool,
}

use super::view::StoredViewKind;

const fn default_view_populated() -> bool {
    true
}

impl Deref for StoredView {
    type Target = StoredViewDefinition;
    fn deref(&self) -> &Self::Target {
        &self.definition
    }
}

impl DerefMut for StoredView {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.definition
    }
}

impl StoredView {
    pub fn security(&self) -> BoundTableSecurity {
        self.security.clone()
    }

    pub fn set_security(&mut self, security: BoundTableSecurity) {
        self.security = security;
    }

    pub fn security_invoker(&self) -> bool {
        self.options.iter().any(|(name, value)| {
            name == "security_invoker" && matches!(value.as_str(), "true" | "on" | "yes" | "1")
        })
    }
}

impl StoredView {
    pub fn rewrite_definition(&self) -> crate::catalog::view::ViewRewriteDefinition {
        crate::catalog::view::ViewRewriteDefinition {
            query: self.query.clone(),
            output_columns: self.output_columns.clone(),
            options: self.options.clone(),
            kind: self.kind,
            materialized_column_types: self.materialized_column_types.clone(),
        }
    }
    pub fn row_schema(
        &self,
        routines: &dyn crate::routines::RoutineResolution,
        catalog: super::analysis::CatalogReadView,
        resolution: super::resolution::RelationNameResolution,
    ) -> Result<crate::RowSchema, crate::SQLError> {
        self.rewrite_definition()
            .row_schema(routines, catalog, resolution)
    }
}

pub mod dependencies;

pub mod restoration;

pub mod references;
pub mod removal;
