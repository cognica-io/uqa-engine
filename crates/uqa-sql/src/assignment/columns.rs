//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared mutation columns, generated-column assignment rules, and table-value coercion.
use super::{conversion::coerce_assignment_value, AssignmentContext};
use crate::{
    ast::{ColumnDef, ColumnType, Expr, GeneratedColumnKind},
    SQLError,
};
use std::collections::BTreeSet;
use uqa_core::{RelationIdentity, Value};
pub type ColumnCatalogError = Box<dyn std::error::Error>;

pub trait AssignmentColumnCatalog {
    fn try_describe_table(&self, table: &str)
        -> Result<Option<Vec<ColumnDef>>, ColumnCatalogError>;
    fn columns_declared(&self, table: &str) -> Result<bool, ColumnCatalogError>;
    fn try_column_insert_default_expr(
        &self,
        table: &str,
        column: &str,
    ) -> Result<Option<Expr>, ColumnCatalogError>;
}
fn dml_storage_error(action: &str, error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("{action} failed in storage backend: {error}"))
}
fn ddl_storage_error(action: &str, error: ColumnCatalogError) -> SQLError {
    crate::catalog::errors::storage_error(action, error.as_ref())
}
/// Coerce a write value to fit the column's declared type.
pub fn coerce_to_column_type(
    assignment: &dyn AssignmentContext,
    catalog: &dyn AssignmentColumnCatalog,
    table: &str,
    column: &str,
    value: Value,
) -> Result<Value, SQLError> {
    coerce_to_column_type_from(assignment, catalog, table, column, value, None)
}

pub fn coerce_to_column_type_from(
    assignment: &dyn AssignmentContext,
    catalog: &dyn AssignmentColumnCatalog,
    table: &str,
    column: &str,
    value: Value,
    source: Option<&ColumnType>,
) -> Result<Value, SQLError> {
    let cols = match catalog
        .try_describe_table(table)
        .map_err(|err| ddl_storage_error("column type coercion", err))?
    {
        Some(c) => c,
        None => return Ok(value),
    };
    let Some(def) = cols.iter().find(|c| c.name == column) else {
        return Ok(value);
    };
    coerce_assignment_value(assignment, value, &def.ty, source)
}

pub fn validate_mutation_columns<'a>(
    catalog: &dyn AssignmentColumnCatalog,
    table: &str,
    columns: impl IntoIterator<Item = &'a str>,
    action: &str,
) -> Result<(), SQLError> {
    let definitions = catalog
        .try_describe_table(table)
        .map_err(|err| dml_storage_error(action, err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    // Programmatically-created document tables intentionally have no SQL
    // schema and retain their open-field behavior. SQL CREATE TABLE always
    // supplies definitions, and those targets must reject misspelled or
    // repeated mutation columns instead of persisting arbitrary fields.
    if definitions.is_empty() {
        let declared = catalog
            .columns_declared(table)
            .map_err(|error| dml_storage_error(action, error))?;
        if !declared {
            return Ok(());
        }
    }
    let known: BTreeSet<&str> = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    let mut seen = BTreeSet::new();
    for column in columns {
        if !seen.insert(column) {
            return Err(SQLError::TypeMismatch(format!(
                "{action}: column `{column}` is specified more than once"
            )));
        }
        if !known.contains(column) {
            let relation = RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
            return Err(SQLError::Routine {
                sqlstate: "42703".into(),
                message: format!(
                    "column \"{column}\" of relation \"{}\" does not exist",
                    relation.name
                ),
            });
        }
    }
    Ok(())
}

pub fn generated_column_kind(
    catalog: &dyn AssignmentColumnCatalog,
    table: &str,
    column: &str,
) -> Result<Option<GeneratedColumnKind>, SQLError> {
    Ok(catalog
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read generated column: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?
        .into_iter()
        .find(|definition| definition.name == column)
        .and_then(|definition| definition.generated.map(|generated| generated.kind)))
}
