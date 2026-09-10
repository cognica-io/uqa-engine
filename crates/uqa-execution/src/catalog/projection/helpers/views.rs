//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live view-schema projection at the catalog adapter boundary.

use crate::catalog::{context::CatalogContext, view::StoredView};
use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_sql::ast::{ColumnDef as SQLColumnDef, ColumnType};
use uqa_sql::SQLError;

pub fn all_schema_names(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<String>, SQLError> {
    Ok(catalog.all_schema_names(resolution))
}

pub fn view_columns_for(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    view: &StoredView,
) -> Result<Vec<SQLColumnDef>, SQLError> {
    let schema =
        context.stored_view_schema_with_catalog(view, catalog.clone(), resolution.clone())?;
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
            not_null_is_local: true,
            auto_increment: None,
            unique: false,
            default: None,
            generated: None,
            check: None,
            check_name: None,
            check_enforced: true,
            check_validated: true,
            check_no_inherit: false,
            check_is_local: true,
            check_object_id: None,
            references: None,
        })
        .collect())
}
