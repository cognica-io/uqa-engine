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
use uqa_core::{memory::ProductionControl, Value};

pub(crate) mod production;
pub use production::{
    declared_column_names_with_control, validate_mutation_columns_with_control,
    validate_mutation_targets_with_control,
};
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
    /// Lend one declaration generation after observing the same default/generated sequence dependencies as a full description. Call the reader exactly once for a found table and do no catalog work afterwards. Consumers inspect names, types and flags; expression consumers still request resolved expressions.
    fn with_column_metadata_with_control(
        &self,
        table: &str,
        control: &ProductionControl<'_>,
        read: &mut dyn FnMut(&[ColumnDef]) -> Result<(), SQLError>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let Some(columns) = self
            .try_describe_table(table)
            .map_err(|error| catalog_error(error, control))?
        else {
            return Ok(false);
        };
        read(&columns)?;
        Ok(true)
    }
    fn columns_declared_with_control(
        &self,
        table: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        self.columns_declared(table)
            .map_err(|error| catalog_error(error, control))
    }
    fn try_column_insert_default_expr(
        &self,
        table: &str,
        column: &str,
    ) -> Result<Option<Expr>, ColumnCatalogError>;
    /// The sequence an identity DEFAULT draws from. An identity declaration with no sequence owned by it reports `PostgreSQL`'s error.
    fn try_identity_column_sequence(
        &self,
        table: &str,
        column: &str,
    ) -> Result<Option<String>, SQLError> {
        Ok(self
            .try_column_shape(table, column)
            .map_err(|error| SQLError::Internal(format!("read identity column: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?
            .and_then(|shape| shape.identity_sequence))
    }
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
fn catalog_error(error: ColumnCatalogError, control: &ProductionControl<'_>) -> SQLError {
    match control.format(format_args!("{error}")) {
        Ok(message) => SQLError::Internal(message.into_parts().0),
        Err(error) => error.into(),
    }
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
    coerce_column_value(assignment, value, &shape.ty, source)
}

/// Admit a table attribute and flatten procedural record descriptors at the tuple
/// boundary. The same representation is used by triggers, constraints and storage.
pub fn coerce_column_value(
    assignment: &dyn AssignmentContext,
    value: Value,
    target: &ColumnType,
    source: Option<&ColumnType>,
) -> Result<Value, SQLError> {
    let value = coerce_assignment_value(assignment, value, target, source)?;
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    crate::expr::composites::flatten_value_with_control(
        control.retain_external_value(value)?,
        &control,
    )
    .map(|value| value.into_uncontrolled().expect("table column assignment"))
}

pub fn validate_mutation_columns<'a>(
    catalog: &dyn AssignmentColumnCatalog,
    table: &str,
    columns: impl IntoIterator<Item = &'a str>,
    action: &str,
) -> Result<(), SQLError> {
    validate_mutation_columns_with_control(
        catalog,
        table,
        columns,
        action,
        &ProductionControl::uncontrolled(),
    )
}

/// The sequence `DEFAULT` draws an identity column's value from, or `None` for a column that is not an identity column.
pub fn identity_column_sequence(
    catalog: &dyn AssignmentColumnCatalog,
    table: &str,
    column: &str,
) -> Result<Option<String>, SQLError> {
    catalog.try_identity_column_sequence(table, column)
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
    validate_mutation_targets_with_control(
        catalog,
        table,
        targets,
        action,
        insert,
        &ProductionControl::uncontrolled(),
    )
}
