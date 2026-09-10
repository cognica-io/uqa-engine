//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind foreign-key declarations, select referenced keys, and validate REFERENCES privileges.
use crate::assignment::columns::{AssignmentColumnCatalog, ColumnCatalogError};
use crate::ast::TableKeyConstraint;
use crate::SQLError;
/// Namespace, key declarations, and REFERENCES checks for one foreign-key declaration.
pub trait ForeignKeyDefinitionCatalog {
    fn resolve_table_reference(&self, name: &str) -> Result<String, SQLError>;
    fn bound_table_name(&self, name: &str) -> Result<Option<String>, SQLError>;
    fn referenceable_keys(
        &self,
        table: &str,
    ) -> Result<Vec<TableKeyConstraint>, ColumnCatalogError>;
    fn ensure_reference_privilege(&self, table: &str, column: &str) -> Result<(), SQLError>;
}
pub struct ForeignKeyDefinitionContext<'a> {
    pub catalog: &'a dyn ForeignKeyDefinitionCatalog,
    pub columns: &'a dyn AssignmentColumnCatalog,
}
fn ddl_storage_error(action: &str, error: ColumnCatalogError) -> SQLError {
    crate::catalog::errors::storage_error(action, error.as_ref())
}
fn constraint_error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}
pub fn validate_foreign_key_definition(
    context: &ForeignKeyDefinitionContext<'_>,
    table: &str,
    foreign_key: &mut crate::ast::ForeignKey,
) -> Result<(), SQLError> {
    validate_foreign_key_definition_with_local_state(context, table, None, None, foreign_key)
}

pub fn validate_foreign_key_definition_with_local_state(
    context: &ForeignKeyDefinitionContext<'_>,
    table: &str,
    local_columns: Option<&[crate::ast::ColumnDef]>,
    local_keys: Option<&[crate::ast::TableKeyConstraint]>,
    foreign_key: &mut crate::ast::ForeignKey,
) -> Result<(), SQLError> {
    foreign_key.ref_table = context
        .catalog
        .resolve_table_reference(&foreign_key.ref_table)?;
    validate_bound_foreign_key_definition_with_local_state(
        context,
        table,
        local_columns,
        local_keys,
        foreign_key,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves DDL dependency and action order"
)]
pub fn validate_bound_foreign_key_definition_with_local_state(
    context: &ForeignKeyDefinitionContext<'_>,
    table: &str,
    local_columns: Option<&[crate::ast::ColumnDef]>,
    local_keys: Option<&[crate::ast::TableKeyConstraint]>,
    foreign_key: &mut crate::ast::ForeignKey,
) -> Result<(), SQLError> {
    let stored_columns;
    let columns = if let Some(columns) = local_columns {
        columns
    } else {
        stored_columns = context
            .columns
            .try_describe_table(table)
            .map_err(|error| ddl_storage_error("FOREIGN KEY local table", error))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        &stored_columns
    };
    for column in &foreign_key.local_columns {
        if !columns.iter().any(|definition| definition.name == *column) {
            return Err(SQLError::UnknownColumn(format!("{table}.{column}")));
        }
    }
    let referenced = context
        .catalog
        .bound_table_name(&foreign_key.ref_table)?
        .ok_or_else(|| SQLError::UnknownTable(foreign_key.ref_table.clone()))?;
    let referenced_columns = context
        .columns
        .try_describe_table(&referenced)
        .map_err(|error| ddl_storage_error("FOREIGN KEY referenced columns", error))?
        .ok_or_else(|| SQLError::UnknownTable(referenced.clone()))?;
    let local = context
        .catalog
        .bound_table_name(table)?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let referenced_keys = if referenced == local {
        match local_keys {
            Some(keys) => keys.to_vec(),
            None => context
                .catalog
                .referenceable_keys(&referenced)
                .map_err(|error| ddl_storage_error("FOREIGN KEY referenced key", error))?,
        }
    } else {
        context
            .catalog
            .referenceable_keys(&referenced)
            .map_err(|error| ddl_storage_error("FOREIGN KEY referenced key", error))?
    };
    if foreign_key.ref_columns.is_empty() {
        let primary_key = referenced_keys
            .iter()
            .find(|key| key.kind == crate::ast::TableKeyConstraintKind::PrimaryKey)
            .ok_or_else(|| {
                constraint_error(
                    "42704",
                    format!("there is no primary key for referenced table \"{referenced}\""),
                )
            })?;
        foreign_key.ref_columns.clone_from(&primary_key.columns);
    }
    if foreign_key.local_columns.len() != foreign_key.ref_columns.len() {
        return Err(constraint_error(
            "42830",
            "number of referencing and referenced columns for foreign key disagree",
        ));
    }
    for (local_column, referenced_column) in foreign_key
        .local_columns
        .iter()
        .zip(&foreign_key.ref_columns)
    {
        let local_definition = columns
            .iter()
            .find(|definition| definition.name == *local_column)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{local_column}")))?;
        let referenced_definition = referenced_columns
            .iter()
            .find(|definition| definition.name == *referenced_column)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{referenced}.{referenced_column}")))?;
        if crate::type_resolution::foreign_key_operand_type(
            &local_definition.ty,
            &referenced_definition.ty,
        )
        .is_err()
        {
            return Err(constraint_error(
                "42804",
                format!(
                    "foreign key constraint cannot be implemented: key columns \"{local_column}\" and \"{referenced_column}\" are of incompatible types: {} and {}",
                    local_definition.ty.sql_name(),
                    referenced_definition.ty.sql_name()
                ),
            ));
        }
    }
    if foreign_key.period {
        super::constraints::validate_foreign_key_definition(
            table,
            columns,
            &referenced,
            &referenced_columns,
            &referenced_keys,
            foreign_key,
        )?;
    } else {
        let referenced_column_set = foreign_key
            .ref_columns
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        let has_unique_key = referenced_column_set.len() == foreign_key.ref_columns.len()
            && referenced_keys.iter().any(|key| {
                key.columns.len() == foreign_key.ref_columns.len()
                    && key
                        .columns
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        == referenced_column_set
            });
        if !has_unique_key {
            return Err(constraint_error(
                "42830",
                format!(
                    "there is no unique constraint matching given keys for referenced table \"{referenced}\""
                ),
            ));
        }
    }
    for column in &foreign_key.ref_columns {
        context
            .catalog
            .ensure_reference_privilege(&referenced, column)?;
    }
    foreign_key.referenced_key = referenced_keys
        .iter()
        .find(|key| {
            key.columns.len() == foreign_key.ref_columns.len()
                && foreign_key
                    .ref_columns
                    .iter()
                    .all(|column| key.columns.contains(column))
                && (!foreign_key.period || key.without_overlaps)
        })
        .and_then(|key| key.name.clone());
    foreign_key.ref_table = referenced;
    Ok(())
}

pub fn column_foreign_key(
    column: &crate::ast::ColumnDef,
    reference: &crate::ast::ForeignKeyRef,
) -> crate::ast::ForeignKey {
    crate::ast::ForeignKey {
        referenced_key: reference.referenced_key.clone(),
        name: reference.name.clone(),
        object_id: reference.object_id,
        local_columns: vec![column.name.clone()],
        ref_table: reference.table.clone(),
        ref_columns: reference.column.iter().cloned().collect(),
        on_update: reference.on_update,
        on_delete: reference.on_delete,
        on_delete_set_columns: Vec::new(),
        match_type: reference.match_type,
        enforced: reference.enforced,
        validated: reference.validated,
        deferrable: reference.deferrable,
        initially_deferred: reference.initially_deferred,
        period: reference.period,
    }
}
