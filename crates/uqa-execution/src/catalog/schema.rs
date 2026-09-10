//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live AGE catalog adaptation for SQL-owned virtual relation schemas.

use super::{CatalogReadView, RelationNameResolution};
use uqa_sql::ast::ColumnType;
pub use uqa_sql::catalog::{
    ag_catalog_domains, ag_catalog_type_oid, age_agtype, age_graphid, information_schema_domains,
    VirtualRelation, AG_CATALOG_SCHEMA,
};

pub fn resolve_virtual_relation(
    resolution: &RelationNameResolution,
    name: &str,
) -> Option<VirtualRelation> {
    uqa_sql::catalog::resolve_virtual_relation(resolution.search_path(), name)
}

pub fn virtual_relation_schema(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
) -> Result<Option<Vec<(String, ColumnType)>>, uqa_sql::SQLError> {
    if let Some(relation) = resolve_virtual_relation(resolution, name) {
        return Ok(Some(relation.schema()));
    }
    super::graph::age_label_relation_schema(catalog, resolution, name)
}

pub fn virtual_relation_accepts_row_lock(
    resolution: &RelationNameResolution,
    name: &str,
) -> Option<bool> {
    resolve_virtual_relation(resolution, name).map(VirtualRelation::accepts_row_lock)
}
