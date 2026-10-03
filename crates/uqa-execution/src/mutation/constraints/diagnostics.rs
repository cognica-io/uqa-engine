//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Render visible index input values with the existing catalog deparser and SQL type output functions.

use super::ConstraintContext;
use crate::catalog::{context::CatalogContext, projection};
use uqa_core::Value;
use uqa_sql::{catalog::index::EnforcedKey, expr::EngineHook, ColumnType, SQLError};

pub(super) fn unique_key_detail(
    context: ConstraintContext<'_>,
    table: &str,
    key: &EnforcedKey,
    values: &[Value],
) -> Result<Option<String>, SQLError> {
    Ok(index_key_detail(context, table, key, values)?
        .map(|detail| format!("{detail} already exists.")))
}

pub(crate) fn duplicate_index_key_detail(
    context: ConstraintContext<'_>,
    table: &str,
    key: &EnforcedKey,
    values: &[Value],
) -> Result<Option<String>, SQLError> {
    Ok(index_key_detail(context, table, key, values)?
        .map(|detail| format!("{detail} is duplicated.")))
}

fn index_key_detail(
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
    let catalog_context = diagnostics.catalog;
    let catalog = catalog_context.catalog_read_view();
    let resolution = catalog_context
        .session_execution_view()
        .relation_name_resolution();
    let definition = key
        .index
        .as_ref()
        .and_then(|identity| catalog.snapshot().definitions.catalog_indexes.get(identity))
        .map(|row| {
            uqa_sql::catalog::index::stored::index_definition(row.definition_json.as_deref())
        })
        .transpose()
        .map_err(|error| SQLError::Internal(format!("index output types: {error}")))?;
    let names = key
        .keys
        .iter()
        .map(|key| projection::index_key_definition(&catalog, &resolution, key, false))
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    let output = OutputNames(catalog_context);
    let values = values
        .iter()
        .enumerate()
        .map(|(position, value)| {
            if *value == Value::Null {
                return Ok("null".into());
            }
            let ty = match definition
                .as_ref()
                .and_then(|definition| definition.key_types.get(position))
            {
                Some(ty) => ty.clone(),
                None => {
                    let column = key.keys[position].column().ok_or_else(|| {
                        SQLError::Internal("stored expression index has no output type".into())
                    })?;
                    context
                        .catalog
                        .column_type(table, column)
                        .map_err(SQLError::Internal)?
                        .ok_or_else(|| SQLError::UnknownColumn(column.into()))?
                }
            };
            uqa_sql::catalog::index::format_key_value(value, &ty, Some(&output))
        })
        .collect::<Result<Vec<_>, SQLError>>()?
        .join(", ");
    Ok(Some(format!("Key ({names})=({values})")))
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
    let output = OutputNames(diagnostics.catalog);
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

/// Prints values with the catalog's names for the types that print them, as type output functions do.
pub(super) struct OutputNames<'a>(pub(super) CatalogContext<'a>);

impl EngineHook for OutputNames<'_> {
    fn resolve_regtype_output(&self, ty: &ColumnType, oid: i64) -> Result<Option<String>, String> {
        projection::resolve_regtype_output(&self.0, ty, oid)
    }

    fn nextval(&self, _name: &str) -> Result<i64, SQLError> {
        Err(SQLError::Internal(
            "index output cannot advance a sequence".into(),
        ))
    }

    fn currval(&self, _name: &str) -> Result<i64, SQLError> {
        Err(SQLError::Internal(
            "index output cannot read a sequence".into(),
        ))
    }

    fn setval(&self, _name: &str, _value: i64, _is_called: bool) -> Result<i64, SQLError> {
        Err(SQLError::Internal(
            "index output cannot change a sequence".into(),
        ))
    }
}
