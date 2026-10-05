//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog descriptions required by static SQL binding.

use super::resolution::RelationNameResolution;
use crate::ast::ColumnDef;
use crate::plan::QueryPlan;
use crate::routines::SQLUserFunction;
use crate::{ColumnType, SQLError};
use std::sync::Arc;

#[derive(Clone)]
pub struct TableDefinition {
    pub columns: Arc<Vec<ColumnDef>>,
    pub columns_declared: bool,
}

#[derive(Clone)]
pub struct ViewDefinition {
    pub query: QueryPlan,
    pub output_columns: Option<Vec<String>>,
    pub materialized: bool,
    pub materialized_column_types: Vec<Option<ColumnType>>,
}

/// Immutable namespace-aware lookup for SQL analysis. This contract exposes definitions only; it grants no row access, mutation, locking, or transaction services.
pub trait AnalysisCatalog: Send + Sync {
    /// Exact revisions of the requested identities in this catalog view. Return `None` only when the catalog has no revision capability; within an available snapshot, include every requested identity and use a missing value for an absent object.
    fn prepared_dependency_snapshot(
        &self,
        _dependencies: &crate::prepared::dependencies::PreparedAnalysisDependencies,
    ) -> Result<Option<crate::prepared::dependencies::PreparedDependencySnapshot>, SQLError> {
        Ok(None)
    }

    /// The OID of a relation successfully selected in this same immutable namespace. The binder calls this after selecting a catalog relation, never for a CTE or an unsuccessful lookup candidate. Catalog fixtures without identities may leave this unavailable.
    fn relation_dependency(
        &self,
        _resolution: &RelationNameResolution,
        _name: &str,
    ) -> Result<Option<u32>, SQLError> {
        Ok(None)
    }

    /// The schemas that exist and that the analyzing role may use, including implicit namespaces, and its creation namespace. Catalogs without namespace state leave this unavailable.
    fn effective_search_path(
        &self,
        _resolution: &RelationNameResolution,
    ) -> Result<Option<super::resolution::EffectiveSearchPath>, SQLError> {
        Ok(None)
    }

    fn table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError>;
    fn table_name_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError>;
    fn view_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<ViewDefinition>, SQLError>;
    fn foreign_table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError>;
    fn sequence_exists(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<bool, SQLError>;
    fn virtual_relation_schema(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<(String, ColumnType)>>, SQLError>;
    fn sql_functions(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError>;

    /// A relation that a query cannot open, such as a composite type's relation: its unqualified name and the kind `PostgreSQL`'s detail names.
    fn unopenable_relation(
        &self,
        _resolution: &RelationNameResolution,
        _name: &str,
    ) -> Result<Option<UnopenableRelation>, SQLError> {
        Ok(None)
    }
}

/// `errdetail_relkind_not_supported`: the detail naming a relation kind that an operation refuses.
#[must_use]
pub fn relkind_not_supported_detail(kind: &str) -> Option<String> {
    let plural = match kind {
        "table" => "tables",
        "index" => "indexes",
        "sequence" => "sequences",
        "view" => "views",
        "materialized view" => "materialized views",
        "composite type" => "composite types",
        "foreign table" => "foreign tables",
        "partitioned table" => "partitioned tables",
        _ => return None,
    };
    Some(format!("This operation is not supported for {plural}."))
}

/// A relation that holds no rows a query or command can read or change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnopenableRelation {
    pub name: String,
    /// The relation kind in the plural, as `errdetail_relkind_not_supported` names it.
    pub kinds: &'static str,
}

impl UnopenableRelation {
    /// `table_open`'s refusal of a relation that is not a table.
    #[must_use]
    pub fn error(&self) -> SQLError {
        SQLError::Diagnostic {
            sqlstate: "42809".into(),
            message: format!("cannot open relation \"{}\"", self.name),
            detail: Some(format!(
                "This operation is not supported for {}.",
                self.kinds
            )),
            hint: None,
        }
    }
}

pub type CatalogReadView = Arc<dyn AnalysisCatalog>;
