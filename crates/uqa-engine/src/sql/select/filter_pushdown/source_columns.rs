//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-snapshot column ownership for filter pushdown.

use std::collections::BTreeMap;

use uqa_sql::SQLError;

use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};

use super::{query_plan_output_columns, SourcePlan, TABLE_OID_COLUMN, XMIN_COLUMN};

pub(super) type ColumnOwners = BTreeMap<String, Option<String>>;

pub(super) fn source_column_owners(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    source: &SourcePlan,
) -> Result<ColumnOwners, SQLError> {
    let mut owners = ColumnOwners::new();
    collect_source_column_owners(catalog, resolution, source, &mut owners)?;
    Ok(owners)
}

fn collect_source_column_owners(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    source: &SourcePlan,
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
            let table = catalog.table_resolved(resolution, name)?;
            let mut columns = table.map_or_else(Vec::new, |table| {
                table
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect()
            });
            if columns.is_empty() {
                columns = catalog
                    .view_resolved(resolution, name)?
                    .and_then(|view| {
                        view.output_columns
                            .clone()
                            .or_else(|| query_plan_output_columns(&view.query))
                    })
                    .unwrap_or_default();
            }
            if columns.is_empty() {
                columns = catalog
                    .foreign_table_resolved(resolution, name)?
                    .map(|table| {
                        table
                            .columns
                            .iter()
                            .map(|column| column.name.clone())
                            .collect()
                    })
                    .unwrap_or_default();
            }
            if table.is_some() {
                columns.push(TABLE_OID_COLUMN.into());
                columns.push(XMIN_COLUMN.into());
            }
            register_column_owners(owners, qualifier, columns);
        }
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            if alias.is_none() {
                collect_source_column_owners(catalog, resolution, left, owners)?;
                collect_source_column_owners(catalog, resolution, right, owners)?;
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
