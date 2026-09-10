//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-snapshot column ownership for filter pushdown.

use std::collections::BTreeMap;

use uqa_sql::SQLError;

use super::FilterPushdownScope;
use uqa_sql::catalog::{analysis::AnalysisCatalog, resolution::RelationNameResolution};

use super::{query_plan_output_columns, SourcePlan, TABLE_OID_COLUMN, XMIN_COLUMN};

pub(super) type ColumnOwners = BTreeMap<String, Option<String>>;

pub(super) fn source_column_owners(
    scope: FilterPushdownScope<'_>,
    source: &SourcePlan,
) -> Result<ColumnOwners, SQLError> {
    let mut owners = ColumnOwners::new();
    collect_source_column_owners(scope, source, &mut owners)?;
    Ok(owners)
}

fn collect_source_column_owners(
    scope: FilterPushdownScope<'_>,
    source: &SourcePlan,
    owners: &mut ColumnOwners,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            column_aliases,
            bound_columns,
            ..
        } => {
            let qualifier = alias.as_deref().unwrap_or(qualifier);
            let mut columns = if (scope.is_visible_cte)(name) {
                Vec::new()
            } else if let Some(columns) = bound_columns {
                let mut columns = columns.clone();
                columns.extend([TABLE_OID_COLUMN.to_string(), XMIN_COLUMN.to_string()]);
                columns
            } else {
                relation_source_columns(scope.catalog, scope.resolution, name)?
            };
            for (column, alias) in columns.iter_mut().zip(column_aliases) {
                column.clone_from(alias);
            }
            register_column_owners(owners, qualifier, columns);
        }
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            if alias.is_none() {
                collect_source_column_owners(scope, left, owners)?;
                collect_source_column_owners(scope, right, owners)?;
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
    catalog: &dyn AnalysisCatalog,
    resolution: &RelationNameResolution,
    name: &str,
) -> Result<Vec<String>, SQLError> {
    if catalog.sequence_exists(resolution, name)? {
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
