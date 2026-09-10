//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adapt immutable engine catalog and CTE snapshots to SQL binding inputs.

use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};
use crate::sql::select::CteScope;
use std::sync::Arc;
use uqa_sql::binding::context::BindingContext;
use uqa_sql::catalog::analysis::{AnalysisCatalog, TableDefinition, ViewDefinition};
use uqa_sql::{ColumnType, SQLError};

pub(super) fn binding_context(ctes: &CteScope) -> Result<BindingContext<'_>, SQLError> {
    Ok(BindingContext {
        catalog: Arc::new(ctes.catalog_read_view()?),
        resolution: ctes.relation_name_resolution()?,
        ctes: ctes
            .rows
            .iter()
            .map(|(name, rows)| {
                let schema = rows.row_schema();
                let schema = ctes.recursive_control_width(name).map_or_else(
                    || schema.clone(),
                    |visible| uqa_sql::binding::hide_recursive_generated_schema(schema, visible),
                );
                (name.clone(), schema)
            })
            .collect(),
        deferred_ctes: ctes.deferred_ctes().clone(),
        non_returning_ctes: ctes.non_returning_ctes.clone(),
        scalar_subqueries: &ctes.scalar_subqueries,
    })
}

impl AnalysisCatalog for CatalogReadView {
    fn table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        CatalogReadView::table_resolved(self, resolution, name).map(|table| {
            table.map(|table| TableDefinition {
                columns: table.columns.clone(),
                columns_declared: table.columns_declared,
            })
        })
    }

    fn table_name_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        CatalogReadView::table_name_resolved(self, resolution, name)
    }

    fn view_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<ViewDefinition>, SQLError> {
        CatalogReadView::view_resolved(self, resolution, name).map(|view| {
            view.map(|view| ViewDefinition {
                query: view.query.clone(),
                output_columns: view.output_columns.clone(),
                materialized: view.kind == crate::StoredViewKind::Materialized,
                materialized_column_types: view.materialized_column_types.clone(),
            })
        })
    }

    fn foreign_table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        CatalogReadView::foreign_table_resolved(self, resolution, name).map(|table| {
            table.map(|table| TableDefinition {
                columns: Arc::new(table.columns.clone()),
                columns_declared: true,
            })
        })
    }

    fn sequence_exists(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<bool, SQLError> {
        CatalogReadView::sequence_resolved(self, resolution, name)
            .map(|sequence| sequence.is_some())
    }

    fn virtual_relation_schema(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<(String, ColumnType)>>, SQLError> {
        crate::sql::virtual_relation_schema(self, resolution, name)
    }

    fn sql_functions(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<Arc<uqa_sql::routines::SQLUserFunction>>>, SQLError> {
        CatalogReadView::sql_functions(self, resolution, name)
    }
}
