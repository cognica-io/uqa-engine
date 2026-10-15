//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column dependencies extracted from catalog expressions.

use uqa_sql::ast::{ColumnDef as SQLColumnDef, Expr};
use uqa_sql::SQLError;

use super::constraints::ConstraintCatalogColumn;

pub fn named_constraint_columns(
    names: &[String],
    columns: &[SQLColumnDef],
    table_name: &str,
) -> Result<Vec<ConstraintCatalogColumn>, SQLError> {
    names
        .iter()
        .map(|name| {
            Ok(ConstraintCatalogColumn {
                name: name.clone(),
                table_ordinal: i64::from(named_column_number(name, columns, table_name)?),
            })
        })
        .collect()
}

pub fn check_constraint_columns(
    expression: &Expr,
    columns: &[SQLColumnDef],
    table_name: &str,
) -> Result<Vec<ConstraintCatalogColumn>, SQLError> {
    uqa_sql::schema::constraints::check_references(expression)
        .into_iter()
        .filter_map(|reference| reference.column_name().map(|name| (reference, name)))
        .map(|(reference, name)| {
            Ok(ConstraintCatalogColumn {
                name: name.to_owned(),
                table_ordinal: i64::from(
                    reference
                        .attribute_number(|name| named_column_number(name, columns, table_name))?,
                ),
            })
        })
        .collect()
}

fn named_column_number(
    name: &str,
    columns: &[SQLColumnDef],
    table_name: &str,
) -> Result<i16, SQLError> {
    let (index, column) = columns
        .iter()
        .enumerate()
        .find(|(_, column)| column.name == name)
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "constraint on table `{table_name}` references missing column `{name}`"
            ))
        })?;
    uqa_sql::catalog::relation_attributes::column_number(column, index)
}
