//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-snapshot column ownership for filter pushdown.

use std::collections::BTreeMap;

use uqa_sql::SQLError;

use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};

use super::{query_plan_output_columns, CteScope, SourcePlan, TABLE_OID_COLUMN, XMIN_COLUMN};

pub(super) type ColumnOwners = BTreeMap<String, Option<String>>;

pub(super) fn source_column_owners(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    source: &SourcePlan,
    ctes: &CteScope,
) -> Result<ColumnOwners, SQLError> {
    let mut owners = ColumnOwners::new();
    collect_source_column_owners(catalog, resolution, source, ctes, &mut owners)?;
    Ok(owners)
}

fn collect_source_column_owners(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    source: &SourcePlan,
    ctes: &CteScope,
    owners: &mut ColumnOwners,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            ..
        } => {
            let qualifier = alias.as_deref().unwrap_or(qualifier);
            let columns = if ctes.is_visible_cte(name) {
                Vec::new()
            } else {
                relation_source_columns(catalog, resolution, name)?
            };
            register_column_owners(owners, qualifier, columns);
        }
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            if alias.is_none() {
                collect_source_column_owners(catalog, resolution, left, ctes, owners)?;
                collect_source_column_owners(catalog, resolution, right, ctes, owners)?;
            }
        }
        SourcePlan::Values {
            rows,
            alias: Some(alias),
            column_aliases,
            ..
        } => {
            let columns = if column_aliases.is_empty() {
                (1..=rows.first().map_or(0, Vec::len))
                    .map(|index| format!("column{index}"))
                    .collect()
            } else {
                column_aliases.clone()
            };
            register_column_owners(owners, alias, columns);
        }
        SourcePlan::Subquery {
            body,
            alias: Some(alias),
            column_aliases,
        } => {
            let columns = if column_aliases.is_empty() {
                query_plan_output_columns(body).unwrap_or_default()
            } else {
                column_aliases.clone()
            };
            register_column_owners(owners, alias, columns);
        }
        SourcePlan::Function {
            alias: Some(alias),
            column_aliases,
            ..
        } if !column_aliases.is_empty() => {
            register_column_owners(owners, alias, column_aliases.clone());
        }
        SourcePlan::FunctionGroup {
            alias: Some(alias),
            column_aliases,
            ..
        } if !column_aliases.is_empty() => {
            register_column_owners(owners, alias, column_aliases.clone());
        }
        SourcePlan::Values { alias: None, .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { alias: None, .. } => {}
    }
    Ok(())
}

fn relation_source_columns(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
) -> Result<Vec<String>, SQLError> {
    if catalog.sequence_resolved(resolution, name)?.is_some() {
        return Ok(vec![
            "last_value".into(),
            "log_cnt".into(),
            "is_called".into(),
        ]);
    }
    if let Some(table) = catalog.table_resolved(resolution, name)? {
        let mut columns = table
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect::<Vec<_>>();
        columns.push(TABLE_OID_COLUMN.into());
        columns.push(XMIN_COLUMN.into());
        return Ok(columns);
    }
    if let Some(view) = catalog.view_resolved(resolution, name)? {
        return Ok(view
            .output_columns
            .clone()
            .or_else(|| query_plan_output_columns(&view.query))
            .unwrap_or_default());
    }
    Ok(catalog
        .foreign_table_resolved(resolution, name)?
        .map(|table| {
            table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect()
        })
        .unwrap_or_default())
}

fn register_column_owners(
    owners: &mut ColumnOwners,
    qualifier: &str,
    columns: impl IntoIterator<Item = String>,
) {
    for column in columns {
        owners
            .entry(column)
            .and_modify(|owner| *owner = None)
            .or_insert_with(|| Some(qualifier.to_string()));
    }
}
