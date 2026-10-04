//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Render visible index input values with the existing catalog deparser and catalog-aware SQL type output functions.

use super::ConstraintContext;
use crate::catalog::{context::CatalogContext, projection};
use uqa_core::Value;
use uqa_sql::{catalog::index::EnforcedKey, ColumnType, SQLError};

pub(super) fn unique_key_detail(
    context: ConstraintContext<'_>,
    table: &str,
    key: &EnforcedKey,
    values: &[Value],
) -> Result<Option<String>, SQLError> {
    Ok(enforced_key_description(context, table, key, values)?
        .map(|key| format!("Key {key} already exists.")))
}

/// `(names)=(values)` for the values of an enforced key, printed with the output types of its index or of its columns; `None` when the current role may not read every key column.
pub(crate) fn enforced_key_description(
    context: ConstraintContext<'_>,
    table: &str,
    key: &EnforcedKey,
    values: &[Value],
) -> Result<Option<String>, SQLError> {
    let diagnostics = context.diagnostics.diagnostic_context();
    if !diagnostics
        .authorization
        .can_view_index_key(table, &key.keys)?
    {
        return Ok(None);
    }
    let catalog = diagnostics.catalog.catalog_read_view();
    let definition = key
        .index
        .as_ref()
        .and_then(|identity| catalog.snapshot().definitions.catalog_indexes.get(identity))
        .map(|row| {
            uqa_sql::catalog::index::stored::index_definition(row.definition_json.as_deref())
        })
        .transpose()
        .map_err(|error| SQLError::Internal(format!("index output types: {error}")))?;
    let types = key
        .keys
        .iter()
        .enumerate()
        .map(|(position, index_key)| {
            match definition
                .as_ref()
                .and_then(|definition| definition.key_types.get(position))
            {
                Some(ty) => Ok(ty.clone()),
                None => {
                    let column = index_key.column().ok_or_else(|| {
                        SQLError::Internal("stored expression index has no output type".into())
                    })?;
                    context
                        .catalog
                        .column_type(table, column)
                        .map_err(SQLError::Internal)?
                        .ok_or_else(|| SQLError::UnknownColumn(column.into()))
                }
            }
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    render_index_key(diagnostics.catalog, &key.keys, &types, values).map(Some)
}

/// `(names)=(values)` for the values of an index's keys, printed with the keys' output types.
fn render_index_key(
    catalog_context: CatalogContext<'_>,
    keys: &[uqa_sql::ast::IndexKey],
    key_types: &[ColumnType],
    values: &[Value],
) -> Result<String, SQLError> {
    let catalog = catalog_context.catalog_read_view();
    let resolution = catalog_context
        .session_execution_view()
        .relation_name_resolution();
    let names = keys
        .iter()
        .map(|key| projection::index_key_definition(&catalog, &resolution, key, false))
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    if values.len() != key_types.len() {
        return Err(SQLError::Internal(
            "index key values do not match the key's output types".into(),
        ));
    }
    let output = projection::CatalogOutput(catalog_context);
    let values = values
        .iter()
        .zip(key_types)
        .map(|(value, ty)| {
            if *value == Value::Null {
                return Ok("null".into());
            }
            uqa_sql::catalog::index::format_key_value(value, ty, Some(&output))
        })
        .collect::<Result<Vec<_>, SQLError>>()?
        .join(", ");
    Ok(format!("({names})=({values})"))
}

impl crate::schema::indexes::unique_build::IndexKeyDescription for ConstraintContext<'_> {
    fn describe_index_key(
        &self,
        table: &str,
        keys: &[uqa_sql::ast::IndexKey],
        key_types: &[ColumnType],
        values: &[Value],
    ) -> Result<Option<String>, SQLError> {
        let diagnostics = self.diagnostics.diagnostic_context();
        if !diagnostics.authorization.can_view_index_key(table, keys)? {
            return Ok(None);
        }
        render_index_key(diagnostics.catalog, keys, key_types, values).map(Some)
    }
}

/// `Key (columns)=(values)` for values of the columns of a foreign key, named as `table`'s columns and printed with the types of `value_table`'s `value_columns`, as `PostgreSQL`'s `ri_ReportViolation` prints a foreign key's key; `None` when the current role may not read every one of `table`'s columns.
pub(crate) fn foreign_key_key(
    context: ConstraintContext<'_>,
    table: &str,
    columns: &[String],
    value_table: &str,
    value_columns: &[String],
    values: &[Value],
) -> Result<Option<String>, SQLError> {
    let diagnostics = context.diagnostics.diagnostic_context();
    let keys = columns
        .iter()
        .map(|column| uqa_sql::ast::IndexKey::Column(column.clone()))
        .collect::<Vec<_>>();
    if !diagnostics.authorization.can_view_index_key(table, &keys)? {
        return Ok(None);
    }
    let output = projection::CatalogOutput(diagnostics.catalog);
    let names = columns
        .iter()
        .map(|column| uqa_sql::expr::quote_ident(column))
        .collect::<Vec<_>>()
        .join(", ");
    let values = value_columns
        .iter()
        .zip(values)
        .map(|(column, value)| {
            if *value == Value::Null {
                return Ok("null".into());
            }
            let ty = context
                .catalog
                .column_type(value_table, column)
                .map_err(SQLError::Internal)?
                .ok_or_else(|| SQLError::UnknownColumn(column.clone()))?;
            uqa_sql::catalog::index::format_key_value(value, &ty, Some(&output))
        })
        .collect::<Result<Vec<_>, SQLError>>()?
        .join(", ");
    Ok(Some(format!("Key ({names})=({values})")))
}
