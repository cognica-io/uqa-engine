//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-column dependency validation and pure declaration rewrites.

#[cfg(test)]
mod tests;
use crate::ast::{ColumnDef, TableCheck};
use crate::schema::dependencies::schema_expr_references_column;

pub fn clear_foreign_column_default(columns: &mut [ColumnDef], column_name: &str) -> bool {
    let Some(column) = columns.iter_mut().find(|column| column.name == column_name) else {
        return false;
    };
    column.default_catalog_oid = None;
    column.default.take().is_some()
}

pub fn remove_foreign_constraint(
    columns: &mut [ColumnDef],
    checks: &mut Vec<TableCheck>,
    constraint_name: &str,
) -> bool {
    if let Some(column) = columns
        .iter_mut()
        .find(|column| column.not_null && column.not_null_name.as_deref() == Some(constraint_name))
    {
        column.not_null = false;
        column.not_null_explicit = false;
        column.not_null_name = None;
        column.not_null_identity = None;
        column.not_null_validated = true;
        column.not_null_no_inherit = false;
        column.not_null_is_local = true;
        return true;
    }
    remove_foreign_check(columns, checks, constraint_name)
}

pub fn remove_foreign_check(
    columns: &mut [ColumnDef],
    checks: &mut Vec<TableCheck>,
    constraint_name: &str,
) -> bool {
    for column in columns {
        if column.check.is_some() && column.check_name.as_deref() == Some(constraint_name) {
            column.check = None;
            column.check_name = None;
            column.check_object_id = None;
            column.check_catalog_oid = None;
            column.check_is_local = true;
            column.check_enforced = true;
            column.check_validated = true;
            column.check_no_inherit = false;
            return true;
        }
    }
    let Some(index) = checks
        .iter()
        .position(|check| check.name.as_deref() == Some(constraint_name))
    else {
        return false;
    };
    checks.remove(index);
    true
}

pub fn validate_foreign_column_removal(
    columns: &[ColumnDef],
    table_name: &str,
    column_name: &str,
) -> Result<(), String> {
    for column in columns {
        if column.name == column_name {
            continue;
        }
        if column
            .default
            .as_ref()
            .is_some_and(|expression| schema_expr_references_column(expression, column_name))
            || column.generated.as_ref().is_some_and(|generated| {
                schema_expr_references_column(&generated.expression, column_name)
            })
        {
            return Err(format!(
                    "cannot drop generated column `{table_name}`.`{column_name}` because column `{}` depends on it",
                    column.name
                ));
        }
    }
    Ok(())
}

pub fn remove_foreign_column(
    columns: &mut Vec<ColumnDef>,
    checks: &mut Vec<TableCheck>,
    column_name: &str,
) {
    checks.extend(
        crate::schema::columns::removal_metadata::remove_column_declarations(columns, column_name),
    );
    crate::schema::columns::removal_metadata::remove_column_checks(checks, column_name);
}

pub use crate::schema::sequences::dependencies::detach_sequence_provenance as detach_foreign_sequence_provenance;
