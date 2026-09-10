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
}

pub type CatalogReadView = Arc<dyn AnalysisCatalog>;
