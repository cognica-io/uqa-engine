//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable relation definitions for SQL binder tests.

use crate::catalog::analysis::{AnalysisCatalog, CatalogReadView, TableDefinition, ViewDefinition};
use crate::catalog::resolution::RelationNameResolution;
use crate::{ColumnType, RelationIdentity, SQLError};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) fn table_definition(columns: Vec<crate::ast::ColumnDef>) -> TableDefinition {
    TableDefinition {
        columns: Arc::new(columns),
        columns_declared: true,
    }
}

pub(crate) fn catalog(tables: BTreeMap<RelationIdentity, TableDefinition>) -> CatalogReadView {
    catalog_with_foreign_tables(tables, BTreeMap::new())
}

pub(super) fn catalog_with_foreign_tables(
    tables: BTreeMap<RelationIdentity, TableDefinition>,
    foreign_tables: BTreeMap<RelationIdentity, TableDefinition>,
) -> CatalogReadView {
    Arc::new(FixtureCatalog {
        tables,
        foreign_tables,
    })
}

struct FixtureCatalog {
    tables: BTreeMap<RelationIdentity, TableDefinition>,
    foreign_tables: BTreeMap<RelationIdentity, TableDefinition>,
}

impl AnalysisCatalog for FixtureCatalog {
    fn table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        Ok(resolution
            .raw_relation_lookup_candidates(name)?
            .iter()
            .find_map(|identity| self.tables.get(identity).cloned()))
    }
    fn table_name_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<String>, SQLError> {
        Ok(resolution
            .raw_relation_lookup_candidates(name)?
            .iter()
            .find(|identity| self.tables.contains_key(identity))
            .map(RelationIdentity::qualified_name))
    }
    fn view_resolved(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<ViewDefinition>, SQLError> {
        Ok(None)
    }
    fn foreign_table_resolved(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<TableDefinition>, SQLError> {
        Ok(resolution
            .raw_relation_lookup_candidates(name)?
            .iter()
            .find_map(|identity| self.foreign_tables.get(identity).cloned()))
    }
    fn sequence_exists(&self, _: &RelationNameResolution, _: &str) -> Result<bool, SQLError> {
        Ok(false)
    }
    fn virtual_relation_schema(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<Vec<(String, ColumnType)>>, SQLError> {
        Ok(
            crate::catalog::resolve_virtual_relation(resolution.search_path(), name)
                .map(crate::catalog::VirtualRelation::schema),
        )
    }
    fn sql_functions(
        &self,
        _: &RelationNameResolution,
        _: &str,
    ) -> Result<Option<Vec<Arc<crate::routines::SQLUserFunction>>>, SQLError> {
        Ok(None)
    }
}

pub(super) fn resolution(
    search_path: Vec<String>,
    temporary_schema: String,
) -> RelationNameResolution {
    RelationNameResolution {
        search_path,
        temporary_schema,
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: crate::catalog::resolution::RelationLookupMode::Dynamic,
    }
}

/// A binding context over a catalog without relations, for tests of expressions that reference no relation.
pub(crate) fn empty_binding_context() -> super::context::BindingContext<'static> {
    super::context::BindingContext {
        catalog: catalog(BTreeMap::new()),
        resolution: resolution(vec!["public".into()], "pg_temp_1".into()),
        ctes: BTreeMap::new(),
        deferred_ctes: BTreeMap::new(),
        non_returning_ctes: std::collections::BTreeSet::new(),
        scalar_subqueries: &[],
    }
}
