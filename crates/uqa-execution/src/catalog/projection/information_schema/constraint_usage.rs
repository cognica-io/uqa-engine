//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint references use the referenced relation's owner and preserve duplicate constraint names.

use super::super::{
    helpers::{
        constraints::{ConstraintCatalogKind, ConstraintCatalogRow},
        rows::{catalog_name, row, str_value},
    },
    CatalogRequest,
};
use crate::catalog::{CatalogReadView, RelationNameResolution};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;
use uqa_sql::{catalog::relation_attributes::column_by_number, ResultRow, SQLError};

#[derive(Clone, Copy)]
pub enum Usage {
    Columns,
    Tables,
}

pub fn build(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    request: &CatalogRequest,
    usage: Usage,
) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    let constraints = super::constraint_rows::relations(catalog, resolution, request)?;
    let constraints = super::constraint_rows::dependency_lookup(constraints);
    for constraint in &constraints {
        if !request.matches_name("constraint_schema", &constraint.schema)
            || !request.matches_name("constraint_name", &constraint.name)
            || matches!(usage, Usage::Tables)
                && matches!(
                    constraint.kind,
                    ConstraintCatalogKind::Check | ConstraintCatalogKind::NotNull
                )
        {
            continue;
        }
        let (schema, name) = constraint.foreign_key.as_ref().map_or(
            (constraint.schema.as_str(), constraint.table.as_str()),
            |foreign| (foreign.schema.as_str(), foreign.table.as_str()),
        );
        if !request.matches_relation(schema, name) {
            continue;
        }
        let Some(table) = catalog
            .snapshot()
            .tables
            .get(&RelationIdentity::new(schema, name))
        else {
            // PostgreSQL includes ordinary and partitioned tables, but excludes foreign relations.
            continue;
        };
        if !catalog.role_is_enabled_for(resolution.current_user(), &table.security.role_owner) {
            continue;
        }
        if matches!(usage, Usage::Tables) {
            rows.push(usage_row(schema, name, constraint));
            continue;
        }
        let columns: BTreeSet<&str> = if let Some(foreign) = &constraint.foreign_key {
            foreign
                .column_ordinals
                .iter()
                .filter_map(|&number| column_by_number(&table.columns, number))
                .map(|column| column.name.as_str())
                .collect()
        } else {
            constraint
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect()
        };
        for column in columns {
            if request.matches_name("column_name", column) {
                let mut result = usage_row(schema, name, constraint);
                result.insert("column_name".into(), str_value(column));
                rows.push(result);
            }
        }
    }
    Ok(rows)
}

fn usage_row(schema: &str, name: &str, constraint: &ConstraintCatalogRow) -> ResultRow {
    row([
        ("table_catalog", catalog_name()),
        ("table_schema", str_value(schema)),
        ("table_name", str_value(name)),
        ("constraint_catalog", catalog_name()),
        ("constraint_schema", str_value(&constraint.schema)),
        ("constraint_name", str_value(&constraint.name)),
    ])
}

#[cfg(test)]
mod tests;
