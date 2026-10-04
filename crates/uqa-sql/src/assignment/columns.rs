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

/// What a written value needs to know of its column.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnShape {
    pub ty: ColumnType,
    pub generated: Option<GeneratedColumnKind>,
    /// The sequence of an identity column, which `DEFAULT` draws its value from.
    pub identity_sequence: Option<String>,
}

/// The sequence an identity column draws its values from: `None` for any other column.
fn identity_sequence(definition: &ColumnDef) -> Option<String> {
    definition
        .auto_increment
        .as_ref()
        .filter(|provenance| provenance.is_identity())
        .and_then(|provenance| provenance.sequence.clone())
}

pub trait AssignmentColumnCatalog {
    fn try_describe_table(&self, table: &str)
        -> Result<Option<Vec<ColumnDef>>, ColumnCatalogError>;
    fn columns_declared(&self, table: &str) -> Result<bool, ColumnCatalogError>;
    fn try_column_insert_default_expr(
        &self,
        table: &str,
        column: &str,
    ) -> Result<Option<Expr>, ColumnCatalogError>;
    /// The type and generated kind of one column: `None` for an unknown table, and an inner `None` for a column the table does not declare. Every written value asks for it, so a catalog answers from the column alone. The default describes the whole table, which copies every column and resolves every default expression.
    fn try_column_shape(
        &self,
        table: &str,
        column: &str,
    ) -> Result<Option<Option<ColumnShape>>, ColumnCatalogError> {
        Ok(self.try_describe_table(table)?.map(|columns| {
            columns
                .into_iter()
                .find(|definition| definition.name == column)
                .map(|definition| ColumnShape {
                    identity_sequence: identity_sequence(&definition),
                    ty: definition.ty,
                    generated: definition.generated.map(|generated| generated.kind),
                })
        }))
    }
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
    let Some(Some(shape)) = catalog
        .try_column_shape(table, column)
        .map_err(|err| ddl_storage_error("column type coercion", err))?
    else {
        return Ok(value);
    };
    coerce_assignment_value(assignment, value, &shape.ty, source)
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

/// The sequence `DEFAULT` draws an identity column's value from, or `None` for a column that is not an identity column.
pub fn identity_column_sequence(
    catalog: &dyn AssignmentColumnCatalog,
    table: &str,
    column: &str,
) -> Result<Option<String>, SQLError> {
    Ok(catalog
        .try_column_shape(table, column)
        .map_err(|error| SQLError::Internal(format!("read identity column: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?
        .and_then(|shape| shape.identity_sequence))
}

pub fn generated_column_kind(
    catalog: &dyn AssignmentColumnCatalog,
    table: &str,
    column: &str,
) -> Result<Option<GeneratedColumnKind>, SQLError> {
    Ok(catalog
        .try_column_shape(table, column)
        .map_err(|error| SQLError::Internal(format!("read generated column: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?
        .and_then(|shape| shape.generated))
}

/// Validate partial targets without rejecting independent writes into the same column.
pub fn validate_mutation_targets<'a, E: 'a>(
    catalog: &dyn AssignmentColumnCatalog,
    table: &str,
    targets: impl IntoIterator<Item = &'a crate::ast::AssignmentTarget<E>>,
    action: &str,
    insert: bool,
) -> Result<(), SQLError> {
    let targets = targets.into_iter().collect::<Vec<_>>();
    super::targets::validate_repeated_targets(targets.iter().copied(), insert)?;
    let mut seen = BTreeSet::new();
    let names = targets
        .iter()
        .map(|target| target.column.as_str())
        .filter(|name| seen.insert(*name));
    validate_mutation_columns(catalog, table, names, action)
}
