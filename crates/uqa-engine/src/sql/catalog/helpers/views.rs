//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live view-schema projection at the catalog adapter boundary.

use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};
use crate::{Engine, StoredView};
use uqa_sql::ast::{ColumnDef as SQLColumnDef, ColumnType};
use uqa_sql::SQLError;

pub(in crate::sql::catalog) fn all_schema_names(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<String>, SQLError> {
    Ok(catalog.all_schema_names(resolution))
}

pub(in crate::sql::catalog) fn view_columns_for(
    engine: &Engine,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    view: &StoredView,
) -> Result<Vec<SQLColumnDef>, SQLError> {
    let schema =
        engine.stored_view_schema_with_catalog(view, catalog.clone(), resolution.clone())?;
    Ok(schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, name)| SQLColumnDef {
            name: schema.public_name(position).unwrap_or(name).to_string(),
            ty: schema
                .column_type(position)
                .cloned()
                .unwrap_or(ColumnType::Text),
            object_id: None,
            missing_value: None,
            primary_key: false,
            not_null: false,
            not_null_explicit: false,
            not_null_name: None,
            not_null_validated: true,
            not_null_no_inherit: false,
            auto_increment: None,
            unique: false,
            default: None,
            generated: None,
            check: None,
            check_name: None,
            check_enforced: true,
            check_validated: true,
            check_no_inherit: false,
            references: None,
        })
        .collect())
}
